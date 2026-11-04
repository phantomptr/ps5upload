import { RuleTester } from "eslint";
import tseslint from "typescript-eslint";
import { afterAll, describe, it } from "vitest";

import plugin from "./require-assert-ok.mjs";

RuleTester.afterAll = afterAll;
RuleTester.describe = describe;
RuleTester.it = it;
RuleTester.itOnly = it.only;

const tester = new RuleTester({ languageOptions: { parser: tseslint.parser } });
const filename = "/repo/client/src/api/x.ts";
const unguarded = [{ messageId: "unguarded" }];

tester.run("require-assert-ok", plugin.rules["require-assert-ok"], {
  valid: [
    {
      filename,
      code: `export async function a(): Promise<{ ok: boolean }> {
        const r = await invoke("a");
        return assertOk(r, "A");
      }`,
    },
    {
      filename,
      code: `export async function b(): Promise<void> { await invoke("b"); }`,
    },
    {
      filename,
      code: `export async function c(): Promise<{ ok: boolean }> {
        const r = await invoke("c");
        if (!r.ok) throw new Error("no");
        return r;
      }`,
    },
    {
      filename,
      code: `/* ok-checked-by-caller */
      export async function d(): Promise<{ ok: boolean }> { return invoke("d"); }`,
    },
    {
      // A parameter that carries `ok` says nothing about the answer.
      filename,
      code: `export async function e(o: { ok: boolean }): Promise<void> { await invoke("e", o); }`,
    },
  ],
  invalid: [
    {
      // The first `{` is inside the return type: this is the shape the rule used to miss.
      filename,
      code: `export async function f(id: string): Promise<{ ok: boolean }> {
        return invoke("f", { id });
      }`,
      errors: unguarded,
    },
    {
      filename,
      code: `interface Ack { ok: boolean }
      export async function g(): Promise<Ack> { return invoke("g"); }`,
      errors: unguarded,
    },
    {
      filename,
      code: `export async function h() {
        return invoke<{ ok: boolean }>("h");
      }`,
      errors: unguarded,
    },
    {
      // An HTTP status check is not a look at the answer's `ok`.
      filename,
      code: `export async function i(): Promise<{ ok: boolean }> {
        const res = await fetch("/i");
        if (!res.ok) throw new Error("http");
        return res.json();
      }`,
      errors: unguarded,
    },
  ],
});
