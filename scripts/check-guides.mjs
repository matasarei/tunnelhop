// Checks that the user guides still match the app (docs/guide/<lang>.md):
//
// - every **bold** span in a guide is a label the app shows, written exactly as in
//   app/locales/<lang>.json ({placeholders} match any text). The guides keep bold for the
//   app's own labels; Windows' labels and emphasis are in italics;
// - every guide has the same sections (## and ###) as the English one, in the same order;
// - every relative link in README.md and the guides points at a tracked file.
//
// Run from anywhere: `node scripts/check-guides.mjs`. Prints one line per problem and exits 1,
// or prints nothing and exits 0.

import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, normalize, relative } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const guides = join(root, "docs", "guide");
const problems = [];

/** Every string in a locale file, nested keys included. */
function labels(lang) {
  const walk = (value) =>
    typeof value === "string" ? [value] : Object.values(value).flatMap(walk);
  return walk(JSON.parse(readFileSync(join(root, "app", "locales", `${lang}.json`), "utf8")));
}

/** A label as a whole-string pattern, each {placeholder} standing for any text. */
function pattern(label) {
  const parts = label.split(/\{[a-z_]+\}/);
  const escaped = parts.map((p) => p.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  return new RegExp(`^${escaped.join(".+")}$`, "u");
}

/** The lines of a markdown file, with fenced code blocks blanked out. */
function prose(path) {
  let fenced = false;
  return readFileSync(path, "utf8")
    .split(/\r?\n/)
    .map((line) => {
      if (line.trimStart().startsWith("```")) {
        fenced = !fenced;
        return "";
      }
      return fenced ? "" : line;
    });
}

/** Bold spans with their line numbers. A span may wrap onto the next line. */
function bold(lines) {
  const text = lines.join("\n");
  const found = [];
  for (const match of text.matchAll(/\*\*([^*]+?)\*\*/gs)) {
    const line = text.slice(0, match.index).split("\n").length;
    found.push({ line, text: match[1].replace(/\s*\n\s*/g, " ") });
  }
  return found;
}

const headings = (lines) => lines.filter((l) => /^#{2,3} /.test(l)).map((l) => l.match(/^#+/)[0]);

const langs = readdirSync(guides)
  .filter((f) => f.endsWith(".md"))
  .map((f) => f.slice(0, -3));
const english = headings(prose(join(guides, "en.md")));

for (const lang of langs) {
  const file = join(guides, `${lang}.md`);
  const shown = relative(root, file).replaceAll("\\", "/");
  const lines = prose(file);
  const patterns = labels(lang).map(pattern);
  for (const span of bold(lines)) {
    if (!patterns.some((p) => p.test(span.text))) {
      problems.push(`${shown}:${span.line}: **${span.text}** is not a label in app/locales/${lang}.json`);
    }
  }
  const own = headings(lines);
  if (own.join(" ") !== english.join(" ")) {
    problems.push(`${shown}: its sections (${own.length}) do not follow en.md's (${english.length})`);
  }
}

const tracked = new Set(
  execFileSync("git", ["ls-files"], { cwd: root, encoding: "utf8" }).split("\n").filter(Boolean),
);
const linking = ["README.md", ...langs.map((l) => `docs/guide/${l}.md`)];
for (const doc of linking) {
  const lines = prose(join(root, doc));
  lines.forEach((line, i) => {
    for (const [, target] of line.matchAll(/\]\(([^)\s]+)\)/g)) {
      if (/^[a-z]+:/i.test(target) || target.startsWith("#")) continue;
      const path = normalize(join(dirname(doc), target.split("#")[0])).replaceAll("\\", "/");
      if (!tracked.has(path) && !tracked.has(`${path}/README.md`)) {
        problems.push(`${doc}:${i + 1}: link to ${target}, which is not a tracked file`);
      }
    }
  });
}

for (const problem of problems) console.log(problem);
process.exit(problems.length > 0 ? 1 : 0);
