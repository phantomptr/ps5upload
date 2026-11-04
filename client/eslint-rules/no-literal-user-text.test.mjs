import { RuleTester } from "eslint";
import tseslint from "typescript-eslint";
import { afterAll, describe, it } from "vitest";

import plugin from "./no-literal-user-text.mjs";

RuleTester.afterAll = afterAll;
RuleTester.describe = describe;
RuleTester.it = it;
RuleTester.itOnly = it.only;

const tester = new RuleTester({ languageOptions: { parser: tseslint.parser } });
const literal = [{ messageId: "literal" }];

tester.run("no-literal-user-text", plugin.rules["no-literal-user-text"], {
  valid: [
    `pushNotification("info", trStatic("k", "Done"), { body: trStatic("b", "Body") });`,
    `pushNotification("info", title, { body });`,
    `pushNotification("info", withConsolePrefix(host, trStatic("k", "Done")));`,
    `registerTask({ kind: "upload", origin: "x", label: trStatic("k", "Go") });`,
    `updateTask(id, { detail: trStatic("k", "Go"), status: "running" });`,
    `useToastStore.getState().push({ tone: "ok", message: tr("k", undefined, "Saved") });`,
    `store.start("kind", label);`,
    // Not one of the lists: `.start` on something else.
    `server.start("kind", "Not a row");`,
    // No words: a separator or a bare value is not text to translate.
    `pushNotification("info", "—");`,
  ],
  invalid: [
    { code: `pushNotification("info", "Done");`, errors: literal },
    { code: "pushNotification(\"info\", `Installed ${n}`);", errors: literal },
    { code: `pushNotification("info", withConsolePrefix(host, "Done"));`, errors: literal },
    { code: `pushNotification("info", ok ? "Done" : t);`, errors: literal },
    { code: `pushNotification("info", t, { body: "Text" });`, errors: literal },
    { code: `registerTask({ label: "Going" });`, errors: literal },
    { code: `beginTask({ detail: "Going" });`, errors: literal },
    { code: `updateTask(id, { stage: "Going" });`, errors: literal },
    { code: `useActivityHistoryStore.getState().start("x", "Uploading");`, errors: literal },
    { code: `useActivityHistoryStore.getState().update(id, { label: "Uploading" });`, errors: literal },
    { code: `useToastStore.getState().push({ message: "Saved" });`, errors: literal },
    { code: `useToastStore.getState().push({ message: m, action: { label: "Open" } });`, errors: literal },
  ],
});
