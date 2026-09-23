import { formatCost, formatInteger, formatRatio, type FormattedValue } from "../format";
import type { SessionModelEffortDto } from "../../data/types";

const sessionIntegerFormatter = new Intl.NumberFormat("en-US", { maximumFractionDigits: 0, useGrouping: true });

export function formatSessionTitle(title: string | null): string {
  return title && title.trim() ? title : "未命名 Session";
}

export function formatSessionProject(project: string | null): string {
  return project && project.trim() ? project : "未识别项目";
}

export function formatSessionIdForDisplay(source: string, id: string): string {
  return source === "antigravity" && id.startsWith("antigravity:") ? id.slice("antigravity:".length) : id;
}

export function formatSessionModel(models: SessionModelEffortDto[]): FormattedValue {
  if (models.length === 0) return { text: "unknown", title: "unknown", accessibleName: "unknown" };
  const first = formatModelWithReasoningEffort(models[0].model, models[0].reasoning_effort, false);
  const text = models.length === 1 ? first : `${first} +${models.length - 1}`;
  const full = models
    .map(({ model, reasoning_effort }) => formatModelWithReasoningEffort(model, reasoning_effort, false))
    .join(", ");
  return { text, title: full, accessibleName: full };
}

export function formatModelWithReasoningEffort(model: string, effort: string | null, mixed: boolean): string {
  return `${model} (${mixed ? "mixed" : effort ?? "—"})`;
}

type DateParts = {
  year: string;
  month: string;
  day: string;
  hour: string;
  minute: string;
  second: string;
};

function dateParts(value: number, timezone: string): DateParts {
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone: timezone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
  }).formatToParts(new Date(value));
  const values = Object.fromEntries(parts.map((part) => [part.type, part.value]));
  return {
    year: values.year,
    month: values.month,
    day: values.day,
    hour: values.hour,
    minute: values.minute,
    second: values.second,
  };
}

export function formatSessionTime(value: number, timezone: string, now = Date.now()): FormattedValue {
  const current = dateParts(now, timezone);
  const target = dateParts(value, timezone);
  const text =
    target.year === current.year && target.month === current.month && target.day === current.day
      ? `${target.hour}:${target.minute}`
      : target.year === current.year
        ? `${target.month}-${target.day} ${target.hour}:${target.minute}`
        : `${target.year}-${target.month}-${target.day} ${target.hour}:${target.minute}`;
  return {
    text,
    title: `${target.year}-${target.month}-${target.day} ${target.hour}:${target.minute}:${target.second}`,
    accessibleName: text,
  };
}

export function formatSessionTimeWithSeconds(value: number, timezone: string, now = Date.now()): FormattedValue {
  const current = dateParts(now, timezone);
  const target = dateParts(value, timezone);
  const time = `${target.hour}:${target.minute}:${target.second}`;
  const text =
    target.year === current.year && target.month === current.month && target.day === current.day
      ? time
      : target.year === current.year
        ? `${target.month}-${target.day} ${time}`
        : `${target.year}-${target.month}-${target.day} ${time}`;
  const title = `${target.year}-${target.month}-${target.day} ${time}`;
  return { text, title, accessibleName: text };
}

export function formatSessionTokenInteger(value: number): FormattedValue {
  if (!Number.isSafeInteger(value) || value < 0) throw new RangeError("Expected a non-negative safe integer");
  const full = String(value);
  const text = sessionIntegerFormatter.format(value);
  return { text, title: full, accessibleName: full };
}

export function formatSessionNullableTokenInteger(value: number | null): FormattedValue {
  if (value === null) return { text: "—", title: "未知", accessibleName: "未知" };
  return formatSessionTokenInteger(value);
}

export { formatCost, formatRatio };
