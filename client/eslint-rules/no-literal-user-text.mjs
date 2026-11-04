/**
 * Custom ESLint rule: no-literal-user-text
 *
 * `no-untranslated-jsx` sees what a component renders. Text a store hands to the notification
 * center, the Tasks list or the Activity list never passes through JSX, so a literal there reaches
 * a Japanese user as English, and the locale gate cannot see it because it never became a key:
 * "Backed up CUSA12345 to USB", "Installed 3 packages", "Deleting 12 items".
 *
 * It flags string and template literals (with words in them) given as:
 *   - pushNotification(level, TITLE, { body: BODY })
 *   - registerTask({ label: LABEL, detail: DETAIL }) and beginTask({ label, detail })
 *   - updateTask(id, { label, detail, stage })
 *   - activity .start(kind, LABEL, ...) and .update(id, { label })
 *   - toast .push({ message, action: { label } })
 *
 * It looks through a ternary, `??`, `||`, and withConsolePrefix(host, TITLE). A call (tr, trStatic,
 * a formatter) or a variable is taken as already translated: a variable's text is the job of
 * whoever built it.
 */

const HAS_WORD = /\p{L}{2,}/u;

/** The literal parts of a value that would reach the user as they are. */
function literalsIn(node) {
  if (!node) return [];
  switch (node.type) {
    case "Literal":
      return typeof node.value === "string" && HAS_WORD.test(node.value) ? [node] : [];
    case "TemplateLiteral":
      return node.quasis.some((q) => HAS_WORD.test(q.value.cooked ?? "")) ? [node] : [];
    case "ConditionalExpression":
      return [...literalsIn(node.consequent), ...literalsIn(node.alternate)];
    case "LogicalExpression":
      return [...literalsIn(node.left), ...literalsIn(node.right)];
    case "TSAsExpression":
    case "TSNonNullExpression":
      return literalsIn(node.expression);
    case "CallExpression":
      return calleeName(node) === "withConsolePrefix" ? literalsIn(node.arguments[1]) : [];
    default:
      return [];
  }
}

function calleeName(call) {
  const c = call.callee;
  if (c.type === "Identifier") return c.name;
  if (c.type === "MemberExpression" && !c.computed && c.property.type === "Identifier") return c.property.name;
  return null;
}

/** The text in `.getState()`-style chains: is `activity` or `toast` in what the call is made on? */
function receiverText(call, source) {
  return call.callee.type === "MemberExpression" ? source.getText(call.callee.object) : "";
}

function propertyValues(obj, names) {
  if (!obj || obj.type !== "ObjectExpression") return [];
  return obj.properties
    .filter(
      (p) =>
        p.type === "Property" &&
        !p.computed &&
        ((p.key.type === "Identifier" && names.includes(p.key.name)) ||
          (p.key.type === "Literal" && names.includes(p.key.value))),
    )
    .map((p) => p.value);
}

const rule = {
  meta: {
    type: "problem",
    docs: {
      description: "Notification titles, task labels and toasts must go through the i18n layer",
    },
    schema: [],
    messages: {
      literal:
        "User-visible text passed to {{target}} is a literal, so it stays English in every language. Add a key to i18n/locales/en.ts and use trStatic(key, fallback, vars) (or tr in a component).",
    },
  },

  create(context) {
    const source = context.sourceCode ?? context.getSourceCode();
    const flag = (nodes, target) => {
      for (const v of nodes) {
        for (const lit of literalsIn(v)) {
          context.report({ node: lit, messageId: "literal", data: { target } });
        }
      }
    };
    return {
      CallExpression(node) {
        const name = calleeName(node);
        const args = node.arguments;
        if (name === "pushNotification") {
          flag([args[1], ...propertyValues(args[2], ["body"])], "a notification");
        } else if (name === "registerTask" || name === "beginTask") {
          flag(propertyValues(args[0], ["label", "detail"]), "a task");
        } else if (name === "updateTask") {
          flag(propertyValues(args[1], ["label", "detail", "stage"]), "a task");
        } else if (name === "start" && /activity/i.test(receiverText(node, source))) {
          flag([args[1]], "an activity row");
        } else if (name === "update" && /activity/i.test(receiverText(node, source))) {
          flag(propertyValues(args[1], ["label"]), "an activity row");
        } else if (name === "push" && /toast/i.test(receiverText(node, source))) {
          const action = propertyValues(args[0], ["action"])[0];
          flag([...propertyValues(args[0], ["message"]), ...propertyValues(action, ["label"])], "a toast");
        }
      },
    };
  },
};

export default {
  rules: { "no-literal-user-text": rule },
};
