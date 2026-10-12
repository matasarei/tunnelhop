// Writes the notices of everything third-party that ships inside TunnelHop's installer:
//
// - THIRD-PARTY-NOTICES.txt: every crate compiled into th-service.exe and th-app.exe (from
//   `cargo about`, configured in about.toml), and every npm package bundled into the app's
//   frontend (the production entries of app/package-lock.json), each with its licence text;
// - THIRD-PARTY-NOTICES-RUST.html: the Rust standard library's own notices, copied as they are
//   from the pinned toolchain, since std is linked into both exes.
//
// Both are committed and installed beside the exes. Run this after any dependency or toolchain
// change and commit the result; `--check` regenerates in memory and fails when the committed
// files are stale, which the release workflow does before building.
//
// Needs cargo-about 0.9.2: `cargo install cargo-about --locked --version 0.9.2 --features cli`.

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const TEXT = "THIRD-PARTY-NOTICES.txt";
const RUST = "THIRD-PARTY-NOTICES-RUST.html";
const RULE = "=".repeat(80);

/**
 * npm packages published without a licence file: their licence and copyright line, as their
 * upstream repository states them. Any other package without one stops generation.
 */
const UPSTREAM = {
  "@fluentui/web-components": {
    copyright: "Copyright (c) Microsoft Corporation.",
    source: "https://github.com/microsoft/fluentui/blob/master/LICENSE",
  },
  "@microsoft/fast-element": {
    copyright: "Copyright (c) Microsoft Corporation. All rights reserved.",
    source: "https://github.com/microsoft/fast/blob/main/LICENSE",
  },
  "@microsoft/focusgroup-polyfill": {
    copyright: "Copyright (c) Microsoft Corporation.",
    source: "https://github.com/microsoft/polyfills/blob/main/LICENSE.txt",
  },
};

/** Line endings as LF and no trailing spaces, so the output is the same on every machine. */
const tidy = (text) =>
  text
    .replace(/\r\n?/g, "\n")
    .split("\n")
    .map((line) => line.trimEnd())
    .join("\n")
    .trim();

function fail(message) {
  console.error(message);
  process.exit(1);
}

const sysroot = execFileSync("rustc", ["--print", "sysroot"], { cwd: root, encoding: "utf8" }).trim();
const rustDoc = join(sysroot, "share", "doc", "rust");

/** The crates, grouped by licence text, from cargo-about. */
function crates() {
  let json;
  try {
    json = execFileSync(
      "cargo",
      ["about", "generate", "--format", "json", "--workspace", "--locked", "--fail", "-c", "about.toml"],
      { cwd: root, encoding: "utf8", maxBuffer: 64 * 1024 * 1024, stdio: ["ignore", "pipe", "pipe"] },
    );
  } catch (e) {
    fail(`cargo about failed:\n${e.stderr || e.message}`);
  }
  return JSON.parse(json).licenses.map((licence) => ({
    title: `${licence.name} (${licence.id})`,
    id: licence.id,
    users: licence.used_by
      .map(({ crate }) => ({
        line: `${crate.name} ${crate.version}`,
        url: `https://crates.io/crates/${crate.name}/${crate.version}`,
      }))
      .sort((a, b) => a.line.localeCompare(b.line, "en")),
    text: tidy(licence.text),
  }));
}

/** The npm packages bundled into the frontend, each with its own licence text. */
function npmPackages() {
  const lock = JSON.parse(readFileSync(join(root, "app", "package-lock.json"), "utf8"));
  const mit = readFileSync(join(rustDoc, "licenses", "MIT.txt"), "utf8");
  return Object.entries(lock.packages)
    .filter(([path, entry]) => path && !entry.dev)
    .map(([path, entry]) => {
      const name = path.replace(/^.*node_modules\//, "");
      const dir = join(root, "app", path);
      const file = existsSync(dir)
        ? readdirSync(dir).find((f) => /^(licen[cs]e|copying)/i.test(f) && !f.endsWith(".spdx"))
        : undefined;
      let text;
      if (file) {
        text = readFileSync(join(dir, file), "utf8");
      } else if (UPSTREAM[name] && entry.license === "MIT") {
        const { copyright, source } = UPSTREAM[name];
        text = `${mit.replace(/^Copyright \(c\) <year> <copyright holders>$/m, copyright)}\n(${source})`;
      } else {
        fail(`app/${path} (${entry.license}) ships no licence file and has no upstream entry in ${TEXT}'s generator`);
      }
      return {
        title: `${name} ${entry.version} (${entry.license})`,
        id: entry.license,
        users: [{ line: `${name} ${entry.version}`, url: `https://www.npmjs.com/package/${name}/v/${entry.version}` }],
        text: tidy(text),
      };
    });
}

function block({ title, id, users, text }) {
  const lines = [RULE, title, "", "Used by:"];
  for (const user of users) lines.push(`  - ${user.line}  ${user.url}`);
  if (id === "MPL-2.0") {
    lines.push("", "The source code of these MPL-2.0 components is available at the addresses above.");
  }
  lines.push("", text, "");
  return lines.join("\n");
}

const byFirstUser = (a, b) =>
  a.id.localeCompare(b.id, "en") || a.users[0].line.localeCompare(b.users[0].line, "en");

const text = [
  "THIRD-PARTY NOTICES",
  "",
  "TunnelHop includes the third-party software listed below. Each component is listed under the",
  "licence it is used under, followed by that licence's text as its authors ship it.",
  "",
  `The Rust standard library, compiled into th-service.exe and th-app.exe, is under MIT OR`,
  `Apache-2.0; its notices are in ${RUST}, next to this file.`,
  "",
  "Generated by scripts/third-party-notices.mjs from Cargo.lock and app/package-lock.json.",
  "Do not edit by hand.",
  "",
  "Rust crates",
  "",
  ...crates().sort(byFirstUser).map(block),
  "npm packages (the app's window)",
  "",
  ...npmPackages().sort(byFirstUser).map(block),
].join("\n");

const rust = readFileSync(join(rustDoc, "COPYRIGHT-library.html"), "utf8");
const outputs = [
  [TEXT, `${tidy(text)}\n`],
  [RUST, rust],
];

if (process.argv.includes("--check")) {
  const stale = outputs.filter(([file, content]) => {
    const path = join(root, file);
    return !existsSync(path) || tidy(readFileSync(path, "utf8")) !== tidy(content);
  });
  if (stale.length > 0) {
    fail(
      `${stale.map(([file]) => file).join(" and ")} ${stale.length > 1 ? "are" : "is"} stale: ` +
        "run `node scripts/third-party-notices.mjs` and commit the result",
    );
  }
} else {
  for (const [file, content] of outputs) writeFileSync(join(root, file), content);
}
