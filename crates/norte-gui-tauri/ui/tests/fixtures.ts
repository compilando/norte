// The host's golden corpus, read AS IS from the Rust tree.
//
// It's the same file that pins the Rust side (`crates/norte-ui-host/tests/
// golden/*.json`). Having both sides read the SAME corpus is the only thing
// that stops `src/types.ts`'s types from drifting from the contract without
// anything going red: a copy here would be a copy that goes stale.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const GOLDEN = resolve(process.cwd(), "../../norte-ui-host/tests/golden");

export function golden(name: string): Record<string, unknown> {
  return JSON.parse(readFileSync(resolve(GOLDEN, name), "utf8")) as Record<
    string,
    unknown
  >;
}

const I18N = resolve(process.cwd(), "../../norte-i18n/i18n");

/**
 * The real catalogue, read from the same `.ftl` the host uses.
 *
 * The tests used to build a catalogue of two made-up keys, so none of them
 * could notice a missing one: `t` answers an absent key with the key itself,
 * and with an invented fixture that's indistinguishable from normal. Eight
 * surfaces painted a literal `hostile-name` because of this.
 *
 * The parser is one line: `key = value`. Fluent has more grammar —
 * attributes, selectors, continuations — and none of it is needed here: what
 * is needed is knowing which keys EXIST and with what plain text.
 */
export function realCatalog(lang: "es" | "en" = "es"): Record<string, string> {
  const ftl = readFileSync(resolve(I18N, `${lang}.ftl`), "utf8");
  const out: Record<string, string> = {};
  for (const line of ftl.split("\n")) {
    const m = /^([a-z][a-z0-9-]*) = (.*)$/.exec(line);
    if (m?.[1] !== undefined && m[2] !== undefined) {
      out[m[1]] = m[2];
    }
  }
  return out;
}
