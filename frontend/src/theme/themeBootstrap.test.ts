import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";

const html = readFileSync(resolve(dirname(fileURLToPath(import.meta.url)), "../../index.html"), "utf8");
const inlineScript = html.match(/<script>\s*([\s\S]*?)\s*<\/script>/)?.[1];
const bootstrapCases: Array<[string | null, boolean]> = [
  [null, true],
  ["dark", true],
  ["light", false],
  ["system", true],
];

class MemoryStorage implements Storage {
  private readonly values = new Map<string, string>();

  get length() {
    return this.values.size;
  }

  clear() {
    this.values.clear();
  }

  getItem(key: string) {
    return this.values.get(key) ?? null;
  }

  key(index: number) {
    return [...this.values.keys()][index] ?? null;
  }

  removeItem(key: string) {
    this.values.delete(key);
  }

  setItem(key: string, value: string) {
    this.values.set(key, value);
  }
}

let fallbackStorage: Storage | undefined;

function testStorage(): Storage {
  try {
    const storage = window.localStorage;
    if (
      storage &&
      typeof storage.clear === "function" &&
      typeof storage.getItem === "function" &&
      typeof storage.key === "function" &&
      typeof storage.removeItem === "function" &&
      typeof storage.setItem === "function"
    ) {
      return storage;
    }
  } catch {
    // jsdom's default opaque origin has no localStorage implementation.
  }

  if (!fallbackStorage) {
    fallbackStorage = new MemoryStorage();
    Object.defineProperty(window, "localStorage", {
      configurable: true,
      value: fallbackStorage,
    });
  }
  return fallbackStorage;
}

function runBootstrap(stored: string | null, storageReadable = true) {
  testStorage().clear();
  document.documentElement.classList.remove("dark");
  if (stored !== null) testStorage().setItem("usagi.theme", stored);
  const getItem = storageReadable
    ? null
    : vi.spyOn(Object.getPrototypeOf(testStorage()), "getItem").mockImplementation(() => {
        throw new DOMException("blocked", "SecurityError");
      });

  expect(inlineScript).toBeTruthy();
  new Function(inlineScript ?? "")();
  getItem?.mockRestore();
  return document.documentElement.classList.contains("dark");
}

afterEach(() => {
  vi.restoreAllMocks();
  testStorage().clear();
  document.documentElement.classList.remove("dark");
});

describe("Theme first-paint bootstrap", () => {
  it.each(bootstrapCases)("maps stored theme %s to dark=%s before React", (stored, dark) => {
    expect(runBootstrap(stored, true)).toBe(dark);
  });

  it("falls back to dark when localStorage cannot be read", () => {
    expect(runBootstrap("light", false)).toBe(true);
  });

  it("executes the inline theme bootstrap before the app root and module", () => {
    const bootstrapAt = html.indexOf("window.localStorage.getItem(\"usagi.theme\")");
    const rootAt = html.indexOf('<div id="root"></div>');
    const moduleAt = html.indexOf('<script type="module" src="/src/main.tsx"></script>');
    expect(bootstrapAt).toBeGreaterThan(-1);
    expect(bootstrapAt).toBeLessThan(rootAt);
    expect(rootAt).toBeLessThan(moduleAt);
  });
});
