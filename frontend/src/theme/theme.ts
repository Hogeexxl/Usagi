export type Theme = "dark" | "light";

export const THEME_STORAGE_KEY = "usagi.theme";
export const DEFAULT_THEME: Theme = "dark";

export function isTheme(value: string | null): value is Theme {
  return value === "dark" || value === "light";
}
