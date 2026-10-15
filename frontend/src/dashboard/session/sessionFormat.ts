import { formatCost, formatIntegerFull, formatRatio, type FormattedValue } from "../format";

export function formatSessionTitle(title: string | null): string {
  return title && title.trim() ? title : "未命名会话";
}

export function formatSessionProject(project: string | null): string {
  return project && project.trim() ? project : "未识别项目";
}

export function formatSessionIdForDisplay(agent: string, id: string): string {
  return agent === "antigravity" && id.startsWith("antigravity:") ? id.slice("antigravity:".length) : id;
}

export function formatSessionModel(models: Array<{ model: string; effort: string | null }>): FormattedValue {
  if (models.length === 0) return { text: "", title: "", accessibleName: "" };
  const full = models
    .map(({ model, effort }) => formatModelWithReasoningEffort(model, effort, false))
    .join(", ");
  return { text: full, title: full, accessibleName: full };
}

export function formatModelWithReasoningEffort(model: string, effort: string | null, mixed: boolean): string {
  return `${model} (${mixed ? "mixed" : effort?.trim() || "—"})`;
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

export function formatSessionTime(value: number | null, timezone: string, now = Date.now()): FormattedValue {
  if (value === null) return { text: "—", title: "未知", accessibleName: "未知" };
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

export function formatSessionTimeWithSeconds(value: number | null, timezone: string, now = Date.now()): FormattedValue {
  if (value === null) return { text: "—", title: "未知", accessibleName: "未知" };
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
  return formatIntegerFull(value);
}

export function formatSessionNullableTokenInteger(value: number | null): FormattedValue {
  if (value === null) return { text: "—", title: "未知", accessibleName: "未知" };
  return formatSessionTokenInteger(value);
}

export { formatCost, formatRatio };
