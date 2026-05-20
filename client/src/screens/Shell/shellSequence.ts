export type ShellSequenceOp = "always" | "and" | "or";

export interface ShellSequencePart {
  op: ShellSequenceOp;
  cmd: string;
}

export function splitShellSequence(input: string): ShellSequencePart[] {
  const parts: ShellSequencePart[] = [];
  let current = "";
  let nextOp: ShellSequenceOp = "always";
  let quote: "'" | '"' | null = null;
  let escaped = false;

  const pushCurrent = () => {
    const cmd = current.trim();
    if (cmd) {
      parts.push({ op: parts.length === 0 ? "always" : nextOp, cmd });
    }
    current = "";
  };

  for (let i = 0; i < input.length; i += 1) {
    const ch = input[i];
    const next = input[i + 1];

    if (escaped) {
      current += ch;
      escaped = false;
      continue;
    }

    if (ch === "\\" && quote !== "'") {
      current += ch;
      escaped = true;
      continue;
    }

    if ((ch === "'" || ch === '"') && (!quote || quote === ch)) {
      quote = quote ? null : ch;
      current += ch;
      continue;
    }

    if (!quote && ch === ";") {
      pushCurrent();
      nextOp = "always";
      continue;
    }

    if (!quote && ch === "&" && next === "&") {
      pushCurrent();
      nextOp = "and";
      i += 1;
      continue;
    }

    // `||` — run the next part only if the previous FAILED. Without this
    // it fell through to the literal-append default and `a || b` was sent
    // as one command, whose handling depended entirely on the remote
    // parser. (A single `|` pipe is left in the command — the payload's
    // builtin shell, not the client, decides what to do with it.)
    if (!quote && ch === "|" && next === "|") {
      pushCurrent();
      nextOp = "or";
      i += 1;
      continue;
    }

    current += ch;
  }

  pushCurrent();
  return parts;
}
