//! Local civil-time range resolution for Spec 05.
//!
//! Named ranges are resolved from one frozen current instant and one system
//! IANA time-zone name. DST overlap uses the earlier instant; gaps use the
//! first valid instant after the gap.

use std::{
    collections::BTreeSet,
    env, fs,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Datelike, Days, NaiveDate};

#[cfg(any(windows, test))]
use chrono::{LocalResult, Offset, TimeZone};

use crate::{api::query::ApiError, usage::aggregate::TimeRange};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeKey {
    Today,
    Yesterday,
    SevenDays,
    ThirtyDays,
    Year,
    Custom,
}

impl RangeKey {
    pub fn parse(value: Option<&str>) -> Result<Self, ApiError> {
        match value {
            Some("today") => Ok(Self::Today),
            Some("yesterday") => Ok(Self::Yesterday),
            Some("7d") => Ok(Self::SevenDays),
            Some("30d") => Ok(Self::ThirtyDays),
            Some("year") => Ok(Self::Year),
            Some("custom") => Ok(Self::Custom),
            _ => Err(ApiError::InvalidRange),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Yesterday => "yesterday",
            Self::SevenDays => "7d",
            Self::ThirtyDays => "30d",
            Self::Year => "year",
            Self::Custom => "custom",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRange {
    pub key: RangeKey,
    pub start_ms: i64,
    pub end_ms: i64,
    pub timezone: String,
}

impl ResolvedRange {
    pub(crate) fn aggregate_range(&self) -> Result<TimeRange, ApiError> {
        TimeRange::new(self.start_ms, self.end_ms).map_err(|_| ApiError::InvalidRange)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedDay {
    pub date: String,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Resolve every local civil day covered by a named range. The returned
/// boundaries are UTC milliseconds, but the day labels and midnight edges are
/// determined exclusively by the range's IANA time zone. SQLite and the
/// frontend never need platform-local time conversion.
pub fn resolve_day_buckets(range: &ResolvedRange) -> Result<Vec<ResolvedDay>, ApiError> {
    #[cfg(windows)]
    {
        return resolve_day_buckets_with_loader(range, EmbeddedZone::load);
    }
    #[cfg(not(windows))]
    {
        resolve_day_buckets_with_loader(range, TzifZone::load)
    }
}

fn resolve_day_buckets_with_loader<L, Z>(
    range: &ResolvedRange,
    loader: L,
) -> Result<Vec<ResolvedDay>, ApiError>
where
    L: FnOnce(&str) -> Result<Z, ApiError>,
    Z: CivilZone,
{
    let zone = loader(&range.timezone)?;
    let start_seconds = range.start_ms.div_euclid(1_000);
    let local_seconds = start_seconds
        .checked_add(i64::from(zone.offset_at(start_seconds)?))
        .ok_or(ApiError::LocalTimeUnavailable)?;
    let mut date = DateTime::from_timestamp(local_seconds, 0)
        .ok_or(ApiError::LocalTimeUnavailable)?
        .date_naive();
    let mut days = Vec::new();
    while days.len() < 400 {
        let start_ms = zone.local_midnight_to_utc_ms(date)?;
        if start_ms >= range.end_ms {
            break;
        }
        let next = date
            .checked_add_days(Days::new(1))
            .ok_or(ApiError::LocalTimeUnavailable)?;
        let end_ms = zone.local_midnight_to_utc_ms(next)?;
        if start_ms < range.start_ms || end_ms > range.end_ms || end_ms <= start_ms {
            return Err(ApiError::LocalTimeUnavailable);
        }
        days.push(ResolvedDay {
            date: date.format("%Y-%m-%d").to_string(),
            start_ms,
            end_ms,
        });
        date = next;
    }
    if days.is_empty() || days.last().is_none_or(|day| day.end_ms != range.end_ms) {
        return Err(ApiError::LocalTimeUnavailable);
    }
    Ok(days)
}

pub fn resolve_system_range(key: RangeKey) -> Result<ResolvedRange, ApiError> {
    if key == RangeKey::Custom {
        return Err(ApiError::InvalidRange);
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ApiError::LocalTimeUnavailable)?
        .as_millis()
        .try_into()
        .map_err(|_| ApiError::LocalTimeUnavailable)?;
    let timezone = system_timezone_name()?;
    resolve_range_at(key, now_ms, &timezone)
}

pub fn resolve_system_custom_range(from: &str, to: &str) -> Result<ResolvedRange, ApiError> {
    let timezone = system_timezone_name()?;
    resolve_custom_range_at(from, to, &timezone)
}

pub fn resolve_custom_range_at(
    from: &str,
    to: &str,
    timezone: &str,
) -> Result<ResolvedRange, ApiError> {
    #[cfg(windows)]
    {
        return resolve_custom_range_at_with_loader(from, to, timezone, EmbeddedZone::load);
    }

    #[cfg(not(windows))]
    {
        resolve_custom_range_at_with_loader(from, to, timezone, TzifZone::load)
    }
}

fn resolve_custom_range_at_with_loader<L, Z>(
    from: &str,
    to: &str,
    timezone: &str,
    loader: L,
) -> Result<ResolvedRange, ApiError>
where
    L: FnOnce(&str) -> Result<Z, ApiError>,
    Z: CivilZone,
{
    let from = parse_custom_date(from)?;
    let to = parse_custom_date(to)?;
    if from > to {
        return Err(ApiError::InvalidRange);
    }
    let end_date = to
        .checked_add_days(Days::new(1))
        .ok_or(ApiError::InvalidRange)?;
    let zone = loader(timezone)?;
    let start_ms = zone.local_midnight_to_utc_ms(from)?;
    let end_ms = zone.local_midnight_to_utc_ms(end_date)?;
    if start_ms >= end_ms {
        return Err(ApiError::InvalidRange);
    }
    Ok(ResolvedRange {
        key: RangeKey::Custom,
        start_ms,
        end_ms,
        timezone: timezone.to_owned(),
    })
}

fn parse_custom_date(value: &str) -> Result<NaiveDate, ApiError> {
    if value.len() != 10
        || value.as_bytes()[4] != b'-'
        || value.as_bytes()[7] != b'-'
        || value
            .as_bytes()
            .iter()
            .enumerate()
            .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return Err(ApiError::InvalidRange);
    }
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| ApiError::InvalidRange)?;
    (date.format("%Y-%m-%d").to_string() == value)
        .then_some(date)
        .ok_or(ApiError::InvalidRange)
}

pub fn resolve_range_at(
    key: RangeKey,
    now_utc_ms: i64,
    timezone: &str,
) -> Result<ResolvedRange, ApiError> {
    #[cfg(windows)]
    {
        return resolve_range_at_with_loader(key, now_utc_ms, timezone, EmbeddedZone::load);
    }

    #[cfg(not(windows))]
    {
        resolve_range_at_with_loader(key, now_utc_ms, timezone, TzifZone::load)
    }
}

fn resolve_range_at_with_loader<L, Z>(
    key: RangeKey,
    now_utc_ms: i64,
    timezone: &str,
    loader: L,
) -> Result<ResolvedRange, ApiError>
where
    L: FnOnce(&str) -> Result<Z, ApiError>,
    Z: CivilZone,
{
    let zone = loader(timezone)?;
    resolve_with_zone(key, now_utc_ms, timezone, &zone)
}

#[cfg(test)]
pub(crate) fn resolve_range_at_with_embedded_loader(
    key: RangeKey,
    now_utc_ms: i64,
    timezone: &str,
) -> Result<ResolvedRange, ApiError> {
    resolve_range_at_with_loader(key, now_utc_ms, timezone, EmbeddedZone::load)
}

#[cfg(test)]
pub(crate) fn resolve_utc_range_at_for_test(
    key: RangeKey,
    now_utc_ms: i64,
) -> Result<ResolvedRange, ApiError> {
    let zone = TzifZone {
        initial_offset: 0,
        transitions: Vec::new(),
        offsets: vec![0],
    };
    resolve_with_zone(key, now_utc_ms, "UTC", &zone)
}

fn resolve_with_zone(
    key: RangeKey,
    now_utc_ms: i64,
    timezone: &str,
    zone: &impl CivilZone,
) -> Result<ResolvedRange, ApiError> {
    let utc_seconds = now_utc_ms.div_euclid(1_000);
    let local_seconds = utc_seconds
        .checked_add(i64::from(zone.offset_at(utc_seconds)?))
        .ok_or(ApiError::LocalTimeUnavailable)?;
    let local_now = DateTime::from_timestamp(local_seconds, 0)
        .ok_or(ApiError::LocalTimeUnavailable)?
        .naive_utc();
    let today = local_now.date();
    let (start_date, end_date) = civil_dates(key, today)?;
    let start_ms = zone.local_midnight_to_utc_ms(start_date)?;
    let end_ms = zone.local_midnight_to_utc_ms(end_date)?;
    if start_ms > end_ms {
        return Err(ApiError::LocalTimeUnavailable);
    }
    Ok(ResolvedRange {
        key,
        start_ms,
        end_ms,
        timezone: timezone.to_owned(),
    })
}

fn civil_dates(key: RangeKey, today: NaiveDate) -> Result<(NaiveDate, NaiveDate), ApiError> {
    let next_day = |date: NaiveDate| {
        date.checked_add_days(Days::new(1))
            .ok_or(ApiError::LocalTimeUnavailable)
    };
    match key {
        RangeKey::Today => Ok((today, next_day(today)?)),
        RangeKey::Yesterday => Ok((
            today
                .checked_sub_days(Days::new(1))
                .ok_or(ApiError::LocalTimeUnavailable)?,
            today,
        )),
        RangeKey::SevenDays => Ok((
            today
                .checked_sub_days(Days::new(6))
                .ok_or(ApiError::LocalTimeUnavailable)?,
            next_day(today)?,
        )),
        RangeKey::ThirtyDays => Ok((
            today
                .checked_sub_days(Days::new(29))
                .ok_or(ApiError::LocalTimeUnavailable)?,
            next_day(today)?,
        )),
        RangeKey::Year => {
            let start = NaiveDate::from_ymd_opt(today.year(), 1, 1)
                .ok_or(ApiError::LocalTimeUnavailable)?;
            let end = NaiveDate::from_ymd_opt(
                today
                    .year()
                    .checked_add(1)
                    .ok_or(ApiError::LocalTimeUnavailable)?,
                1,
                1,
            )
            .ok_or(ApiError::LocalTimeUnavailable)?;
            Ok((start, end))
        }
        RangeKey::Custom => Err(ApiError::InvalidRange),
    }
}

fn system_timezone_name() -> Result<String, ApiError> {
    if let Some(name) = env::var_os("TZ").and_then(|value| value.into_string().ok()) {
        return system_timezone_name_from_provider(|| Ok(name));
    }
    system_timezone_name_from_provider(system_timezone_from_os)
}

fn system_timezone_name_from_provider(
    provider: impl FnOnce() -> Result<String, ()>,
) -> Result<String, ApiError> {
    let name = provider().map_err(|_| ApiError::LocalTimeUnavailable)?;
    valid_timezone_name(&name)
        .then_some(name)
        .ok_or(ApiError::LocalTimeUnavailable)
}

#[cfg(windows)]
fn system_timezone_from_os() -> Result<String, ()> {
    iana_time_zone::get_timezone().map_err(|_| ())
}

#[cfg(not(windows))]
fn system_timezone_from_os() -> Result<String, ()> {
    let localtime = fs::read_link("/etc/localtime").map_err(|_| ())?;
    timezone_name_from_path(&localtime).ok_or(())
}

fn timezone_name_from_path(path: &Path) -> Option<String> {
    let components = path.components().collect::<Vec<_>>();
    let marker = components.iter().rposition(
        |component| matches!(component, Component::Normal(value) if *value == "zoneinfo"),
    )?;
    let name = components[marker + 1..]
        .iter()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    valid_timezone_name(&name).then_some(name)
}

fn valid_timezone_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+'))
        })
}

trait CivilZone {
    fn offset_at(&self, utc_seconds: i64) -> Result<i32, ApiError>;
    fn local_midnight_to_utc_ms(&self, date: NaiveDate) -> Result<i64, ApiError>;
}

#[derive(Clone, Copy, Debug)]
struct ZoneTransition {
    utc_seconds: i64,
    offset_seconds: i32,
}

#[derive(Clone, Debug)]
struct TzifZone {
    initial_offset: i32,
    transitions: Vec<ZoneTransition>,
    offsets: Vec<i32>,
}

impl CivilZone for TzifZone {
    fn offset_at(&self, utc_seconds: i64) -> Result<i32, ApiError> {
        TzifZone::offset_at(self, utc_seconds)
    }

    fn local_midnight_to_utc_ms(&self, date: NaiveDate) -> Result<i64, ApiError> {
        TzifZone::local_midnight_to_utc_ms(self, date)
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug)]
struct EmbeddedZone {
    timezone: chrono_tz::Tz,
}

#[cfg(any(windows, test))]
impl EmbeddedZone {
    fn load(name: &str) -> Result<Self, ApiError> {
        if !valid_timezone_name(name) {
            return Err(ApiError::LocalTimeUnavailable);
        }
        let timezone = name.parse().map_err(|_| ApiError::LocalTimeUnavailable)?;
        Ok(Self { timezone })
    }
}

#[cfg(any(windows, test))]
impl CivilZone for EmbeddedZone {
    fn offset_at(&self, utc_seconds: i64) -> Result<i32, ApiError> {
        let utc = DateTime::from_timestamp(utc_seconds, 0).ok_or(ApiError::LocalTimeUnavailable)?;
        Ok(self
            .timezone
            .offset_from_utc_datetime(&utc.naive_utc())
            .fix()
            .local_minus_utc())
    }

    fn local_midnight_to_utc_ms(&self, date: NaiveDate) -> Result<i64, ApiError> {
        let local = date
            .and_hms_opt(0, 0, 0)
            .ok_or(ApiError::LocalTimeUnavailable)?;
        let utc_seconds = match self.timezone.from_local_datetime(&local) {
            LocalResult::Single(value) => value.timestamp(),
            LocalResult::Ambiguous(first, second) => first.timestamp().min(second.timestamp()),
            LocalResult::None => chrono_tz::GapInfo::new(&local, &self.timezone)
                .and_then(|gap| gap.end.map(|value| value.timestamp()))
                .ok_or(ApiError::LocalTimeUnavailable)?,
        };
        utc_seconds
            .checked_mul(1_000)
            .ok_or(ApiError::LocalTimeUnavailable)
    }
}

impl TzifZone {
    fn load(name: &str) -> Result<Self, ApiError> {
        if !valid_timezone_name(name) {
            return Err(ApiError::LocalTimeUnavailable);
        }
        let path = [
            PathBuf::from("/usr/share/zoneinfo").join(name),
            PathBuf::from("/var/db/timezone/zoneinfo").join(name),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or(ApiError::LocalTimeUnavailable)?;
        let bytes = fs::read(path).map_err(|_| ApiError::LocalTimeUnavailable)?;
        Self::parse(&bytes)
    }

    fn parse(bytes: &[u8]) -> Result<Self, ApiError> {
        let first = TzifHeader::parse(bytes, 0)?;
        let (header, block_start, time_width) = if matches!(first.version, b'2' | b'3' | b'4') {
            let second_header = 44_usize
                .checked_add(first.block_len(4)?)
                .ok_or(ApiError::LocalTimeUnavailable)?;
            (
                TzifHeader::parse(bytes, second_header)?,
                second_header
                    .checked_add(44)
                    .ok_or(ApiError::LocalTimeUnavailable)?,
                8,
            )
        } else {
            (first, 44, 4)
        };
        header.parse_block(bytes, block_start, time_width)
    }

    fn offset_at(&self, utc_seconds: i64) -> Result<i32, ApiError> {
        let index = self
            .transitions
            .partition_point(|transition| transition.utc_seconds <= utc_seconds);
        Ok(index
            .checked_sub(1)
            .map(|index| self.transitions[index].offset_seconds)
            .unwrap_or(self.initial_offset))
    }

    fn local_midnight_to_utc_ms(&self, date: NaiveDate) -> Result<i64, ApiError> {
        let midnight = date
            .and_hms_opt(0, 0, 0)
            .ok_or(ApiError::LocalTimeUnavailable)?;
        let local_seconds = midnight.and_utc().timestamp();
        let mut candidates = self
            .offsets
            .iter()
            .filter_map(|offset| {
                let utc = local_seconds.checked_sub(i64::from(*offset))?;
                (self.offset_at(utc).ok()? == *offset).then_some(utc)
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        candidates.dedup();
        let utc_seconds = if let Some(earliest) = candidates.first().copied() {
            earliest
        } else {
            self.gap_end(local_seconds)
                .ok_or(ApiError::LocalTimeUnavailable)?
        };
        utc_seconds
            .checked_mul(1_000)
            .ok_or(ApiError::LocalTimeUnavailable)
    }

    fn gap_end(&self, local_seconds: i64) -> Option<i64> {
        let mut prior = self.initial_offset;
        for transition in &self.transitions {
            let next = transition.offset_seconds;
            if next > prior {
                let before = transition.utc_seconds.checked_add(i64::from(prior))?;
                let after = transition.utc_seconds.checked_add(i64::from(next))?;
                if (before..after).contains(&local_seconds) {
                    return Some(transition.utc_seconds);
                }
            }
            prior = next;
        }
        None
    }
}

#[derive(Clone, Copy)]
struct TzifHeader {
    version: u8,
    ttisgmtcnt: usize,
    ttisstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

impl TzifHeader {
    fn parse(bytes: &[u8], start: usize) -> Result<Self, ApiError> {
        let header = bytes
            .get(
                start
                    ..start
                        .checked_add(44)
                        .ok_or(ApiError::LocalTimeUnavailable)?,
            )
            .ok_or(ApiError::LocalTimeUnavailable)?;
        if &header[..4] != b"TZif" {
            return Err(ApiError::LocalTimeUnavailable);
        }
        let count = |offset| {
            let raw: [u8; 4] = header[offset..offset + 4]
                .try_into()
                .map_err(|_| ApiError::LocalTimeUnavailable)?;
            usize::try_from(u32::from_be_bytes(raw)).map_err(|_| ApiError::LocalTimeUnavailable)
        };
        let value = Self {
            version: header[4],
            ttisgmtcnt: count(20)?,
            ttisstdcnt: count(24)?,
            leapcnt: count(28)?,
            timecnt: count(32)?,
            typecnt: count(36)?,
            charcnt: count(40)?,
        };
        if value.typecnt == 0 || value.typecnt > 256 {
            return Err(ApiError::LocalTimeUnavailable);
        }
        Ok(value)
    }

    fn block_len(self, time_width: usize) -> Result<usize, ApiError> {
        self.timecnt
            .checked_mul(time_width)
            .and_then(|value| value.checked_add(self.timecnt))
            .and_then(|value| value.checked_add(self.typecnt.checked_mul(6)?))
            .and_then(|value| value.checked_add(self.charcnt))
            .and_then(|value| value.checked_add(self.leapcnt.checked_mul(time_width + 4)?))
            .and_then(|value| value.checked_add(self.ttisstdcnt))
            .and_then(|value| value.checked_add(self.ttisgmtcnt))
            .ok_or(ApiError::LocalTimeUnavailable)
    }

    fn parse_block(
        self,
        bytes: &[u8],
        start: usize,
        time_width: usize,
    ) -> Result<TzifZone, ApiError> {
        let end = start
            .checked_add(self.block_len(time_width)?)
            .ok_or(ApiError::LocalTimeUnavailable)?;
        let block = bytes
            .get(start..end)
            .ok_or(ApiError::LocalTimeUnavailable)?;
        let times_bytes = self
            .timecnt
            .checked_mul(time_width)
            .ok_or(ApiError::LocalTimeUnavailable)?;
        let mut times = Vec::with_capacity(self.timecnt);
        for chunk in block[..times_bytes].chunks_exact(time_width) {
            let value = if time_width == 8 {
                i64::from_be_bytes(
                    chunk
                        .try_into()
                        .map_err(|_| ApiError::LocalTimeUnavailable)?,
                )
            } else {
                i64::from(i32::from_be_bytes(
                    chunk
                        .try_into()
                        .map_err(|_| ApiError::LocalTimeUnavailable)?,
                ))
            };
            times.push(value);
        }
        let indices = block
            .get(times_bytes..times_bytes + self.timecnt)
            .ok_or(ApiError::LocalTimeUnavailable)?;
        let types_start = times_bytes + self.timecnt;
        let types_end = types_start
            .checked_add(self.typecnt * 6)
            .ok_or(ApiError::LocalTimeUnavailable)?;
        let types = block
            .get(types_start..types_end)
            .ok_or(ApiError::LocalTimeUnavailable)?;
        let mut offsets = Vec::with_capacity(self.typecnt);
        for chunk in types.as_chunks::<6>().0 {
            offsets.push(i32::from_be_bytes(
                chunk[..4]
                    .try_into()
                    .map_err(|_| ApiError::LocalTimeUnavailable)?,
            ));
        }
        let initial_offset = offsets[0];
        let transitions = times
            .into_iter()
            .zip(indices.iter().copied())
            .map(|(utc_seconds, index)| {
                let offset_seconds = offsets
                    .get(usize::from(index))
                    .copied()
                    .ok_or(ApiError::LocalTimeUnavailable)?;
                Ok(ZoneTransition {
                    utc_seconds,
                    offset_seconds,
                })
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        let offsets = offsets
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(TzifZone {
            initial_offset,
            transitions,
            offsets,
        })
    }
}

#[cfg(test)]
#[path = "range/tests/spec05_p2.rs"]
mod spec05_p2;

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    #[test]
    fn timezone_provider_validates_injected_system_names() {
        assert_eq!(
            system_timezone_name_from_provider(|| Ok("America/New_York".to_owned())),
            Ok("America/New_York".to_owned())
        );
        assert_eq!(
            system_timezone_name_from_provider(|| Ok("../escape".to_owned())),
            Err(ApiError::LocalTimeUnavailable)
        );
        assert_eq!(
            system_timezone_name_from_provider(|| Err(())),
            Err(ApiError::LocalTimeUnavailable)
        );
    }

    #[test]
    fn embedded_provider_resolves_named_dst_boundaries_without_tzif_files() {
        let timezone =
            system_timezone_name_from_provider(|| Ok("America/Havana".to_owned())).unwrap();
        let range = resolve_range_at_with_embedded_loader(
            RangeKey::Today,
            DateTime::parse_from_rfc3339("2026-03-08T12:00:00Z")
                .unwrap()
                .timestamp_millis(),
            &timezone,
        )
        .unwrap();
        assert_eq!(range.timezone, "America/Havana");
        assert_eq!(
            range.start_ms,
            DateTime::parse_from_rfc3339("2026-03-08T05:00:00Z")
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(
            range.end_ms,
            DateTime::parse_from_rfc3339("2026-03-09T04:00:00Z")
                .unwrap()
                .timestamp_millis()
        );

        let lord_howe = resolve_range_at_with_embedded_loader(
            RangeKey::Today,
            DateTime::parse_from_rfc3339("2026-10-04T12:00:00Z")
                .unwrap()
                .timestamp_millis(),
            "Australia/Lord_Howe",
        )
        .unwrap();
        assert_eq!(
            lord_howe.start_ms,
            DateTime::parse_from_rfc3339("2026-10-03T13:30:00Z")
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(
            lord_howe.end_ms,
            DateTime::parse_from_rfc3339("2026-10-04T13:00:00Z")
                .unwrap()
                .timestamp_millis()
        );

        let apia = resolve_range_at_with_embedded_loader(
            RangeKey::Yesterday,
            DateTime::parse_from_rfc3339("2011-12-30T12:00:00Z")
                .unwrap()
                .timestamp_millis(),
            "Pacific/Apia",
        )
        .unwrap();
        assert_eq!(
            apia.start_ms,
            DateTime::parse_from_rfc3339("2011-12-30T10:00:00Z")
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(apia.start_ms, apia.end_ms);
    }

    #[test]
    fn embedded_loader_rejects_invalid_timezone_names() {
        for timezone in ["", "/absolute", "../escape", "Missing/Timezone"] {
            assert_eq!(
                resolve_range_at_with_embedded_loader(RangeKey::Today, 0, timezone),
                Err(ApiError::LocalTimeUnavailable)
            );
        }
    }

    #[test]
    fn custom_range_includes_the_to_date_and_uses_local_midnights() {
        let range = resolve_custom_range_at_with_loader(
            "2026-03-08",
            "2026-03-09",
            "America/New_York",
            EmbeddedZone::load,
        )
        .unwrap();
        assert_eq!(range.key, RangeKey::Custom);
        assert_eq!(
            range.start_ms,
            DateTime::parse_from_rfc3339("2026-03-08T05:00:00Z")
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(
            range.end_ms,
            DateTime::parse_from_rfc3339("2026-03-10T04:00:00Z")
                .unwrap()
                .timestamp_millis()
        );
    }

    #[test]
    fn custom_range_rejects_invalid_dates_and_reversed_ranges() {
        for (from, to) in [
            ("2026-02-30", "2026-03-01"),
            ("2026-03-02", "2026-03-01"),
            ("2026-03-01", "2026-03-01T00:00:00"),
        ] {
            assert_eq!(
                resolve_custom_range_at_with_loader(from, to, "UTC", EmbeddedZone::load),
                Err(ApiError::InvalidRange)
            );
        }
    }

    fn utc_range(key: RangeKey) -> ResolvedRange {
        let now = DateTime::parse_from_rfc3339("2026-08-08T12:34:56Z")
            .unwrap()
            .timestamp_millis();
        resolve_utc_range_at_for_test(key, now).unwrap()
    }

    fn synthetic_havana_zone() -> TzifZone {
        let transition = |value: &str, offset_seconds| ZoneTransition {
            utc_seconds: DateTime::parse_from_rfc3339(value).unwrap().timestamp(),
            offset_seconds,
        };
        TzifZone {
            initial_offset: -18_000,
            transitions: vec![
                transition("2026-03-08T05:00:00Z", -14_400),
                transition("2026-11-01T05:00:00Z", -18_000),
            ],
            offsets: vec![-18_000, -14_400],
        }
    }

    #[test]
    fn named_range_matrix_covers_calendar_boundaries_and_transition_rules() {
        let expected = [
            (
                RangeKey::Today,
                "2026-08-08T00:00:00Z",
                "2026-08-09T00:00:00Z",
            ),
            (
                RangeKey::Yesterday,
                "2026-08-07T00:00:00Z",
                "2026-08-08T00:00:00Z",
            ),
            (
                RangeKey::SevenDays,
                "2026-08-02T00:00:00Z",
                "2026-08-09T00:00:00Z",
            ),
            (
                RangeKey::ThirtyDays,
                "2026-07-10T00:00:00Z",
                "2026-08-09T00:00:00Z",
            ),
            (
                RangeKey::Year,
                "2026-01-01T00:00:00Z",
                "2027-01-01T00:00:00Z",
            ),
        ];
        for (key, start, end) in expected {
            let range = utc_range(key);
            assert_eq!(
                range.start_ms,
                DateTime::parse_from_rfc3339(start)
                    .unwrap()
                    .timestamp_millis()
            );
            assert_eq!(
                range.end_ms,
                DateTime::parse_from_rfc3339(end)
                    .unwrap()
                    .timestamp_millis()
            );
        }

        // Named `today` exercises the same midnight gap/overlap rules with a
        // deterministic transition fixture rather than host tzdb files.
        let havana_gap_now = DateTime::parse_from_rfc3339("2026-03-08T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        let havana_zone = synthetic_havana_zone();
        let havana_gap = resolve_with_zone(
            RangeKey::Today,
            havana_gap_now,
            "America/Havana",
            &havana_zone,
        )
        .unwrap();
        assert_eq!(
            havana_gap.start_ms,
            DateTime::parse_from_rfc3339("2026-03-08T05:00:00Z")
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(
            havana_gap.end_ms,
            DateTime::parse_from_rfc3339("2026-03-09T04:00:00Z")
                .unwrap()
                .timestamp_millis()
        );
        let havana_overlap_now = DateTime::parse_from_rfc3339("2026-11-01T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        let havana_overlap = resolve_with_zone(
            RangeKey::Today,
            havana_overlap_now,
            "America/Havana",
            &havana_zone,
        )
        .unwrap();
        assert_eq!(
            havana_overlap.start_ms,
            DateTime::parse_from_rfc3339("2026-11-01T04:00:00Z")
                .unwrap()
                .timestamp_millis()
        );
        assert_eq!(
            havana_overlap.end_ms,
            DateTime::parse_from_rfc3339("2026-11-02T05:00:00Z")
                .unwrap()
                .timestamp_millis()
        );

        let synthetic = TzifZone {
            initial_offset: 0,
            transitions: vec![
                ZoneTransition {
                    utc_seconds: 86_400,
                    offset_seconds: 3_600,
                },
                ZoneTransition {
                    utc_seconds: 169_200,
                    offset_seconds: 0,
                },
                ZoneTransition {
                    utc_seconds: 259_200,
                    offset_seconds: 86_400,
                },
            ],
            offsets: vec![0, 3_600, 86_400],
        };
        let gap_date = NaiveDate::from_ymd_opt(1970, 1, 2).unwrap();
        assert_eq!(
            synthetic.local_midnight_to_utc_ms(gap_date).unwrap(),
            86_400_000
        );
        let local = 169_200;
        let candidates = synthetic
            .offsets
            .iter()
            .filter_map(|offset| {
                let utc = local - i64::from(*offset);
                (synthetic.offset_at(utc).ok()? == *offset).then_some(utc)
            })
            .collect::<Vec<_>>();
        assert_eq!(candidates.into_iter().min(), Some(165_600));
        let skipped = NaiveDate::from_ymd_opt(1970, 1, 4).unwrap();
        let following = NaiveDate::from_ymd_opt(1970, 1, 5).unwrap();
        assert_eq!(
            synthetic.local_midnight_to_utc_ms(skipped).unwrap(),
            synthetic.local_midnight_to_utc_ms(following).unwrap()
        );

        assert_eq!(RangeKey::parse(None), Err(ApiError::InvalidRange));
        assert_eq!(
            RangeKey::parse(Some("quarter")),
            Err(ApiError::InvalidRange)
        );
        for timezone in ["", "/absolute", "../escape", "Missing/Timezone"] {
            assert_eq!(
                resolve_range_at(RangeKey::Today, 0, timezone),
                Err(ApiError::LocalTimeUnavailable)
            );
        }
        assert_eq!(
            resolve_range_at(RangeKey::Today, i64::MAX, "Asia/Shanghai"),
            Err(ApiError::LocalTimeUnavailable)
        );
    }
}
