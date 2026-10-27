import { invoke } from "@tauri-apps/api/core";
import ko from "../locales/ko.json";
import en from "../locales/en.json";

const dictionaries: Record<string, Record<string, string>> = { ko, en };

export const locale = $state({ lang: "ko" });

/** Load the language the backend resolved from settings / the OS. */
export async function loadLanguage() {
  locale.lang = await invoke<string>("get_language");
  document.documentElement.lang = locale.lang;
}

/** Translate `key`, filling `{name}` placeholders from `args`. */
export function t(key: string, args: Record<string, string | number> = {}): string {
  const s = dictionaries[locale.lang]?.[key] ?? key;
  return Object.entries(args).reduce((acc, [k, v]) => acc.replaceAll(`{${k}}`, String(v)), s);
}
