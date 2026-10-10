// The window's text, in the language the Rust side chose from Windows' display language. The
// texts are the files under `locales`, one per language, which the tray reads too. Only the
// app's own text is here: what the service says arrives in English and is shown as it is.
import de from "../../locales/de.json";
import en from "../../locales/en.json";
import es from "../../locales/es.json";
import fr from "../../locales/fr.json";
import it from "../../locales/it.json";
import ja from "../../locales/ja.json";
import pl from "../../locales/pl.json";
import ptBR from "../../locales/pt-BR.json";
import uk from "../../locales/uk.json";
import zhHans from "../../locales/zh-Hans.json";

/** Every text the app has. English is the list the other languages are checked against. */
export type Key = keyof typeof en;
/** A language has every text; of the forms for one thing, only those its numbers ask for. */
type Texts = Record<Exclude<Key, `${string}_one`>, string> & Partial<Record<Key, string>>;

/** A text with a form per number: `bundles.servers` for `bundles.servers_one`, `_other`, … */
export type CountKey = CountBase<Key>;
type CountBase<K> = K extends `${infer Base}_other` ? Base : never;

// Named as the Rust side names them: the file names under `locales`.
const languages: Record<string, Texts> = {
  en,
  uk,
  es,
  fr,
  de,
  "pt-BR": ptBR,
  it,
  pl,
  "zh-Hans": zhHans,
  ja,
};

let texts: Texts = en;
let plurals = new Intl.PluralRules("en");
/** Chinese and Japanese put nothing between two sentences. */
let between = " ";

/** Speak `tag` from now on; English when the app does not have it. Called once, before the page is drawn. */
export function setLanguage(tag: string) {
  const known = tag in languages ? tag : "en";
  texts = languages[known];
  plurals = new Intl.PluralRules(known);
  between = known === "ja" || known.startsWith("zh") ? "" : " ";
  document.documentElement.lang = known;
}

type Values = Record<string, string | number>;

// One pass, so a value that itself reads `{name}` is never filled in again.
const fill = (template: string, values?: Values) =>
  values ? template.replace(/\{(\w+)\}/g, (whole, name) => (name in values ? String(values[name]) : whole)) : template;

/** The text for `key`, with `values` put in its `{placeholders}`. */
export const t = (key: Key, values?: Values) => fill(texts[key] ?? en[key] ?? key, values);

/** The form of `key` that goes with `n` in this language; `{n}` is filled in with it. */
export function tn(key: CountKey, n: number, values?: Values) {
  const all = texts as Record<string, string>;
  const form = all[`${key}_${plurals.select(n)}`] ?? all[`${key}_other`] ?? key;
  return fill(form, { ...values, n });
}

/** Whole sentences one after another, without those that do not apply. */
export const sentences = (...parts: (string | false | null | undefined)[]) => parts.filter(Boolean).join(between);
