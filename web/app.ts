const TOKEN_KEY = "vsrocq.proof.token";
const THEME_KEY = "vsrocq.proof.theme";
const COMMAND_LABELS: Record<Command, string> = {
  "step-backward": "Step backward",
  "step-forward": "Step forward",
  "run-to-end": "Run to end",
  interrupt: "Interrupt",
  reset: "Reset",
};
const GROUPS: readonly [GoalGroupKey, string, string][] = [
  ["goals", "Focused", "No focused goals."],
  ["unfocusedGoals", "Unfocused", "No unfocused goals."],
  ["shelvedGoals", "Shelved", "No shelved goals."],
  ["givenUpGoals", "Given up", "No given-up goals."],
];
const LEVELS: Record<number, readonly [string, string]> = {
  1: ["error", "Error"],
  2: ["warning", "Warning"],
  3: ["information", "Information"],
  4: ["hint", "Hint"],
};
const PP_TAGS: Record<string, true> = {
  "constr-notation": true,
  "constr-keyword": true,
  "constr-evar": true,
  "constr-path": true,
  "constr-reference": true,
  "constr-type": true,
  "constr-variable": true,
  "message-debug": true,
  "message-error": true,
  "message-warning": true,
  "module-keyword": true,
  "tactic-keyword": true,
  "tactic-primitive": true,
  "tactic-string": true,
  "diff-added": true,
  "diff-added-bg": true,
  "diff-removed": true,
  "diff-removed-bg": true,
};
type Command = "step-backward" | "step-forward" | "run-to-end" | "interrupt" | "reset";
type GoalGroupKey = "goals" | "unfocusedGoals" | "shelvedGoals" | "givenUpGoals";
type ProofStatus = "idle" | "checking" | "ready" | "error";
type ConnectionKind = "connecting" | "connected" | "retrying" | "missing" | "unauthorized";
type RawObject = Record<string, unknown>;
type Message = [number, unknown];

interface BridgeState {
  revision: number;
  document: { uri: string; name: string; version: number } | null;
  status: ProofStatus;
  proof: RawObject | null;
  error: string | null;
  toolchain: { missing: boolean; message: string; url: string | null } | null;
}

interface GoalGroup {
  key: GoalGroupKey;
  label: string;
  empty: string;
  goals: RawObject[];
}

interface ProofModel {
  active: boolean;
  groups: GoalGroup[];
  count: number;
  messages: Message[];
  range: RawObject | null;
}

interface BoxStartToken {
  kind: "box-start";
  mode: string;
  indent: number;
  width: number;
}

type PrettyToken =
  | BoxStartToken
  | { kind: "box-end" }
  | { kind: "tag-start"; className: string }
  | { kind: "tag-end" }
  | { kind: "text"; text: string }
  | { kind: "newline" }
  | { kind: "break"; spaces: number; indent: number; followingWidth: number };
type PrettyWork = PrettyToken | { kind: "node"; value: unknown };

function isCommand(value: unknown): value is Command {
  return typeof value === "string" && Object.hasOwn(COMMAND_LABELS, value);
}

function node<K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, text?: unknown): HTMLElementTagNameMap[K] {
  const result = document.createElement(tag);
  if (className) result.className = className;
  if (text !== undefined) result.textContent = String(text);
  return result;
}

function byId(id: string): HTMLElement {
  const element = document.getElementById(id);
  if (!element) throw new Error(`Missing proof view element: ${id}`);
  return element;
}

function record(value: unknown): RawObject | null {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as RawObject : null;
}

function optionRecord(value: unknown): RawObject | null {
  return record(value) || (Array.isArray(value) && value.length === 1 ? record(value[0]) : null);
}

function optionString(value: unknown): string | null {
  if (typeof value === "string") return value;
  return Array.isArray(value) && value.length === 1 && typeof value[0] === "string" ? value[0] : null;
}

function plainText(value: unknown): string {
  if (value == null) return "";
  return typeof value === "string" ? value : (JSON.stringify(value) ?? String(value));
}

function integer(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isSafeInteger(value) ? value : fallback;
}

function characters(text: string): number {
  let count = 0;
  for (const character of text) count += character === "\t" ? 4 : 1;
  return count;
}

function flatTextWidth(text: string): number {
  return /[\r\n]/.test(text) ? Infinity : characters(text);
}

// VsRocq's Printing.pp variants are arrays, not HTML. Compile once, then lay
// out the same tokens again when the available monospace width changes.
function compilePretty(value: unknown): PrettyToken[] {
  const tokens: PrettyToken[] = [];
  const work: PrettyWork[] = [{ kind: "node", value }];
  while (work.length) {
    const item = work.pop()!;
    if (item.kind !== "node") {
      tokens.push(item);
      continue;
    }
    const tree = item.value;
    if (tree == null) continue;
    if (!Array.isArray(tree)) {
      if (tree === "Ppcmd_empty") continue;
      if (tree === "Ppcmd_force_newline") tokens.push({ kind: "newline" });
      else tokens.push({ kind: "text", text: plainText(tree) });
      continue;
    }
    switch (tree[0]) {
      case "Ppcmd_empty":
        break;
      case "Ppcmd_string":
        tokens.push({ kind: "text", text: plainText(tree[1]) });
        break;
      case "Ppcmd_glue": {
        const children = Array.isArray(tree[1]) ? tree[1] : [];
        for (let i = children.length - 1; i >= 0; i--) work.push({ kind: "node", value: children[i] });
        break;
      }
      case "Ppcmd_box": {
        const box = Array.isArray(tree[1]) ? tree[1] : [tree[1]];
        const open: BoxStartToken = { kind: "box-start", mode: typeof box[0] === "string" ? box[0] : "Pp_hovbox", indent: integer(box[1]), width: 0 };
        tokens.push(open);
        work.push({ kind: "box-end" });
        work.push({ kind: "node", value: tree[2] });
        break;
      }
      case "Ppcmd_tag": {
        const tag = typeof tree[1] === "string" ? tree[1].replaceAll(".", "-") : "";
        if (Object.hasOwn(PP_TAGS, tag)) {
          tokens.push({ kind: "tag-start", className: `pp-${tag}` });
          work.push({ kind: "tag-end" });
        }
        work.push({ kind: "node", value: tree[2] });
        break;
      }
      case "Ppcmd_print_break":
        tokens.push({ kind: "break", spaces: Math.max(0, integer(tree[1])), indent: integer(tree[2]), followingWidth: 0 });
        break;
      case "Ppcmd_force_newline":
        tokens.push({ kind: "newline" });
        break;
      case "Ppcmd_comment":
        tokens.push({ kind: "tag-start", className: "pp-comment" });
        tokens.push({ kind: "text", text: Array.isArray(tree[1]) ? tree[1].map(plainText).join("\n") : plainText(tree[1]) });
        tokens.push({ kind: "tag-end" });
        break;
      default:
        // An untagged sequence is distinct from a tagged Ppcmd_glue node.
        // Preserve unknown data as text instead of silently losing it.
        if (tree.every((child) => Array.isArray(child))) {
          for (let i = tree.length - 1; i >= 0; i--) work.push({ kind: "node", value: tree[i] });
        } else {
          tokens.push({ kind: "text", text: plainText(tree) });
        }
    }
  }

  const boxes: { open: BoxStartToken; width: number }[] = [];
  let width = 0;
  for (const token of tokens) {
    if (token.kind === "box-start") {
      boxes.push({ open: token, width });
      width = 0;
    } else if (token.kind === "box-end") {
      const parent = boxes.pop()!;
      parent.open.width = width;
      width += parent.width;
    } else if (token.kind === "text") width += flatTextWidth(token.text);
    else if (token.kind === "break") width += token.spaces;
    else if (token.kind === "newline") width = Infinity;
  }

  // For a packing box, a break needs the width of the next chunk in that
  // box. A nested box is one chunk; its own breaks are laid out separately.
  const following: number[] = [];
  let chunk = 0;
  for (let i = tokens.length - 1; i >= 0; i--) {
    const token = tokens[i];
    if (token.kind === "box-end") {
      following.push(chunk);
      chunk = 0;
    } else if (token.kind === "box-start") chunk = following.pop()! + token.width;
    else if (token.kind === "break") {
      token.followingWidth = chunk;
      chunk = 0;
    } else if (token.kind === "text") chunk += flatTextWidth(token.text);
    else if (token.kind === "newline") chunk = Infinity;
  }
  return tokens;
}

function layoutPretty(tokens: readonly PrettyToken[], columns: number): DocumentFragment {
  const result = document.createDocumentFragment();
  const tags: (DocumentFragment | HTMLSpanElement)[] = [result];
  const boxes = [{ mode: "Pp_hovbox", indent: 0 }];
  let column = 0;
  const emit = (text: string) => {
    if (!text) return;
    tags[tags.length - 1].append(document.createTextNode(text));
    const lastNewline = Math.max(text.lastIndexOf("\n"), text.lastIndexOf("\r"));
    column = lastNewline === -1 ? column + characters(text) : characters(text.slice(lastNewline + 1));
  };
  const newline = (indent: number) => emit(`\n${" ".repeat(Math.max(0, Math.min(columns - 1, indent)))}`);
  for (const token of tokens) {
    const context = boxes[boxes.length - 1];
    switch (token.kind) {
      case "text":
        emit(token.text);
        break;
      case "tag-start": {
        const span = node("span", token.className);
        tags[tags.length - 1].append(span);
        tags.push(span);
        break;
      }
      case "tag-end":
        tags.pop();
        break;
      case "box-start": {
        let mode = token.mode;
        if (mode === "Pp_hvbox") mode = column + token.width <= columns ? "Pp_hbox" : "Pp_vbox";
        boxes.push({ mode, indent: column + token.indent });
        break;
      }
      case "box-end":
        boxes.pop();
        break;
      case "break":
        if (context.mode === "Pp_vbox" || (context.mode !== "Pp_hbox" && column + token.spaces + token.followingWidth > columns)) {
          newline(context.indent + token.indent);
        } else emit(" ".repeat(token.spaces));
        break;
      case "newline":
        newline(context.indent);
        break;
    }
  }
  return result;
}

class PrettyPrinter {
  readonly entries = new Map<HTMLPreElement, { tokens: PrettyToken[]; columns: number }>();
  readonly measure: CanvasRenderingContext2D | null;
  readonly observer: ResizeObserver;

  constructor() {
    this.measure = document.createElement("canvas").getContext("2d");
    this.observer = new ResizeObserver((entries) => {
      for (const entry of entries) {
        if (entry.target instanceof HTMLPreElement) this.layout(entry.target, entry.contentRect.width);
      }
    });
  }

  create(value: unknown, label: string): HTMLPreElement {
    const pre = node("pre", "pp");
    pre.tabIndex = 0;
    pre.setAttribute("aria-label", label);
    const tokens = compilePretty(value);
    pre.append(layoutPretty(tokens, 88));
    this.entries.set(pre, { tokens, columns: 88 });
    this.observer.observe(pre);
    return pre;
  }

  layout(pre: HTMLPreElement, width: number): void {
    const entry = this.entries.get(pre);
    if (!entry || width <= 0) return;
    const style = getComputedStyle(pre);
    let characterWidth = 8;
    if (this.measure) {
      this.measure.font = style.font;
      characterWidth = this.measure.measureText("0000000000").width / 10 || characterWidth;
    }
    characterWidth += Number.parseFloat(style.letterSpacing) || 0;
    const columns = Math.max(12, Math.floor(width / characterWidth));
    if (columns === entry.columns) return;
    entry.columns = columns;
    pre.replaceChildren(layoutPretty(entry.tokens, columns));
  }

  clear(): void {
    this.observer.disconnect();
    this.entries.clear();
  }
}

function collectGoals(value: unknown): RawObject[] {
  const goals: RawObject[] = [];
  const work: unknown[] = [value];
  while (work.length) {
    const item = work.pop();
    if (Array.isArray(item)) {
      // Unfocused goals may be a flat list or nested lists of before/after
      // focus frames. Do not mistake the enclosing lists for goal objects.
      for (let i = item.length - 1; i >= 0; i--) work.push(item[i]);
    } else {
      const goal = record(item);
      if (goal && Object.hasOwn(goal, "goal")) goals.push(goal);
      else if (goal) {
        if (Object.hasOwn(goal, "after")) work.push(goal.after);
        if (Object.hasOwn(goal, "before")) work.push(goal.before);
      }
    }
  }
  return goals;
}

function hypothesisPretty(hypothesis: unknown): unknown {
  const declaration = record(hypothesis);
  if (!declaration) return hypothesis;
  const names = Array.isArray(declaration.ids) ? declaration.ids.map(plainText).join(", ") : "";
  const pieces: unknown[] = [["Ppcmd_tag", "constr.variable", ["Ppcmd_string", names]]];
  const body = optionString(declaration.body);
  if (body !== null) pieces.push(["Ppcmd_string", ` := ${body}`]);
  pieces.push(["Ppcmd_string", ` : ${plainText(declaration._type)}`]);
  return ["Ppcmd_glue", pieces];
}

function proofModel(raw: unknown): ProofModel {
  const params = record(raw);
  const proof = optionRecord(params?.proof) || optionRecord(params?.pp_proof);
  const focusedGoals = collectGoals(proof?.goals);
  const focusedIds = new Set<unknown>(focusedGoals.map((goal) => goal.id).filter((id) => id != null));
  const groups = GROUPS.map(([key, label, empty]) => {
    const goals = key === "goals" ? focusedGoals : collectGoals(proof?.[key]);
    // Proof.background_subgoals includes focused goals. Expose only the
    // remaining background goals in the separate Unfocused group.
    return {
      key, label, empty,
      goals: key === "unfocusedGoals" ? goals.filter((goal) => !focusedIds.has(goal.id)) : goals,
    };
  });
  const rawMessages = params?.messages;
  const plainMessages = params?.pp_messages;
  const richMessages: unknown[] = Array.isArray(rawMessages) ? rawMessages : [];
  const messages: unknown[] = richMessages.length ? richMessages : (Array.isArray(plainMessages) ? plainMessages : []);
  return {
    active: proof !== null,
    groups,
    count: groups.reduce((count, group) => count + group.goals.length, 0),
    messages: messages.filter((message): message is Message => Array.isArray(message) && message.length >= 2 && typeof message[0] === "number"),
    range: record(params?.range),
  };
}

function emptyState(title: string, kind = ""): HTMLDivElement {
  const container = node("div", `empty-state ${kind}`.trim());
  container.append(node("h3", "", title));
  return container;
}

// Rendering owns no network operations and accepts complete shared-state
// snapshots. The proof model can be reused by another renderer.
class ProofView {
  state: BridgeState | null = null;
  pending: Command | null = null;
  connection: ConnectionKind = "connecting";
  documentUri: string | null | undefined = null;
  readonly disclosures = new Map<string, boolean>();
  readonly printer = new PrettyPrinter();
  readonly buttons = [...document.querySelectorAll<HTMLButtonElement>("[data-command]")];

  constructor({ onCommand, onReconnect }: { onCommand: (command: Command) => void; onReconnect: () => void }) {
    for (const button of this.buttons) button.addEventListener("click", () => {
      const command = button.dataset.command;
      if (isCommand(command)) onCommand(command);
    });
    byId("reconnect").addEventListener("click", onReconnect);
    byId("dismiss-error").addEventListener("click", () => {
      byId("command-notice").hidden = true;
    });
    byId("goals-content").addEventListener("toggle", (event) => {
      if (event.target instanceof HTMLDetailsElement && event.target.dataset.disclosure) {
        this.disclosures.set(event.target.dataset.disclosure, event.target.open);
      }
    }, true);
  }

  announce(message: string): void {
    const announcer = byId("announcer");
    if (announcer.textContent !== message) announcer.textContent = message;
  }

  setConnection(kind: ConnectionKind, message = ""): void {
    this.connection = kind;
    const notice = byId("connection-notice");
    const failedAuth = kind === "missing" || kind === "unauthorized";
    notice.hidden = kind === "connected" || kind === "connecting";
    notice.classList.toggle("danger", failedAuth);
    byId("connection-title").textContent = failedAuth ? "Open a fresh proof view from Zed" : "Connection lost";
    byId("connection-message").textContent = message;
    byId("reconnect").hidden = failedAuth;
    if (failedAuth && !this.state) {
      byId("goals-content").replaceChildren(emptyState("Open the proof view from Zed"));
    }
    this.updateHeader();
    this.updateControls();
    if (kind !== "connected" && kind !== "connecting") this.announce(message);
  }

  updateHeader(): void {
    let status: ProofStatus | "disconnected" = this.state?.status || "idle";
    let label = { idle: "Idle", checking: "Checking", ready: "Ready", error: "Error" }[status];
    if (this.pending) {
      status = "checking";
      label = this.pending === "reset" ? "Resetting" : "Checking";
    }
    if (this.connection === "connecting") label = "Connecting";
    else if (this.connection !== "connected") {
      status = "disconnected";
      label = "Disconnected";
    }
    const badge = byId("status");
    badge.dataset.status = status;
    badge.title = label;
    badge.setAttribute("aria-label", label);
    byId("goals-panel").setAttribute("aria-busy", String(status === "checking"));
  }

  canCommand(command: Command): boolean {
    const state = this.state;
    return this.connection === "connected" && !this.pending && state !== null &&
      state.document !== null && !state.toolchain?.missing &&
      (command !== "interrupt" || state.status === "checking");
  }

  updateControls(): void {
    for (const button of this.buttons) {
      const command = button.dataset.command;
      button.disabled = !isCommand(command) || !this.canCommand(command);
      button.setAttribute("aria-busy", String(this.pending === button.dataset.command));
    }
  }

  beginCommand(command: Command): void {
    this.pending = command;
    byId("command-notice").hidden = true;
    byId("server-notice").hidden = true;
    this.printer.clear();
    byId("goals-content").replaceChildren(emptyState(
      command === "reset" ? "Resetting" : "Checking", "checking",
    ));
    byId("messages-content").replaceChildren(node("p", "empty-messages", "Waiting for messages"));
    byId("goal-count").textContent = "0";
    byId("message-count").textContent = "0";
    byId("proof-location").textContent = "";
    byId("message-summary").textContent = "";
    this.updateHeader();
    this.updateControls();
    this.announce(`${COMMAND_LABELS[command]} requested. The previous proof view has been cleared.`);
  }

  commandFailed(message: string): void {
    this.pending = null;
    if (this.state) this.render(this.state);
    byId("command-error").textContent = message;
    byId("command-notice").hidden = false;
    this.announce(message);
  }

  disclosure(key: string, className: string, open = true): HTMLDetailsElement {
    const details = node("details", className);
    details.dataset.disclosure = key;
    details.open = this.disclosures.get(key) ?? open;
    return details;
  }

  render(state: BridgeState): void {
    this.state = state;
    this.pending = null;
    const documentInfo = state.document;
    if (documentInfo?.uri !== this.documentUri) {
      this.disclosures.clear();
      this.documentUri = documentInfo?.uri;
    }
    const filename = documentInfo?.name || "No file selected";
    document.title = documentInfo ? `${filename} - Proof view - vsrocq` : "Proof view - vsrocq";
    const documentName = byId("document-name");
    documentName.textContent = filename;
    documentName.title = documentInfo?.uri || "";
    this.renderNotices(state);
    this.printer.clear();
    const model = proofModel(state.proof);
    this.renderGoals(model, state);
    this.renderMessages(model.messages, state);
    this.updateHeader();
    this.updateControls();
    const liveKeys = new Set([...document.querySelectorAll<HTMLDetailsElement>("[data-disclosure]")].map((details) => details.dataset.disclosure));
    for (const key of this.disclosures.keys()) if (!liveKeys.has(key)) this.disclosures.delete(key);
    const status = { idle: "Idle", checking: "Checking", ready: "Ready", error: "Error" }[state.status];
    this.announce(`${filename}. ${status}. ${model.count} ${model.count === 1 ? "goal" : "goals"}, ${model.messages.length} ${model.messages.length === 1 ? "message" : "messages"}.`);
  }

  renderNotices(state: BridgeState): void {
    byId("server-notice").hidden = !state.error;
    byId("server-error").textContent = state.error || "";
    const toolchain = state.toolchain;
    byId("toolchain-notice").hidden = !toolchain?.missing;
    byId("toolchain-message").textContent = toolchain?.message || "The local language server could not be started.";
    let setupUrl = "https://github.com/rocq-prover/vsrocq#installation";
    if (toolchain?.url) {
      try {
        const url = new URL(toolchain.url);
        if (url.protocol === "https:" && !url.username && !url.password &&
            (url.hostname === "rocq-prover.org" || (url.hostname === "github.com" && url.pathname.startsWith("/rocq-prover/")))) {
          setupUrl = url.href;
        }
      } catch { /* Keep the official setup link for an invalid URL. */ }
    }
    byId("toolchain-link").setAttribute("href", setupUrl);
  }

  renderGoals(model: ProofModel, state: BridgeState): void {
    const container = byId("goals-content");
    byId("goal-count").textContent = String(model.count);
    const end = record(model.range?.end);
    byId("proof-location").textContent = end && typeof end.line === "number" && Number.isSafeInteger(end.line) && end.line >= 0 ? `Line ${end.line + 1}` : "";
    if (!state.document) {
      container.replaceChildren(emptyState("Open a Rocq file in Zed"));
      return;
    }
    if (state.toolchain?.missing) {
      container.replaceChildren(emptyState("Set up the local toolchain"));
      return;
    }
    if (!state.proof) {
      const title = state.status === "checking" ? "Checking"
        : state.status === "error" ? "Proof state unavailable" : "Save in Zed or step forward";
      container.replaceChildren(emptyState(title, state.status === "checking" ? "checking" : ""));
      return;
    }
    if (!model.active) {
      container.replaceChildren(emptyState("Not in proof mode"));
      return;
    }
    const fragment = document.createDocumentFragment();
    if (model.count === 0) fragment.append(emptyState("No remaining goals", "complete"));
    for (const group of model.groups) {
      const details = this.disclosure(`group:${group.key}`, "goal-group", group.goals.length > 0);
      details.dataset.group = group.key;
      const summary = node("summary", "group-summary");
      summary.append(node("span", "group-title", group.label), node("span", "count", group.goals.length));
      details.append(summary);
      if (group.goals.length === 0) details.append(node("p", "group-empty", group.empty));
      else group.goals.forEach((goal, index) => details.append(this.renderGoal(goal, group.key, index)));
      fragment.append(details);
    }
    container.replaceChildren(fragment);
  }

  renderGoal(goal: RawObject, groupKey: GoalGroupKey, index: number): HTMLDetailsElement {
    const id = goal.id == null ? String(index + 1) : String(goal.id);
    const key = `goal:${groupKey}:${id}:${index}`;
    const details = this.disclosure(key, "goal-card");
    const summary = node("summary", "goal-summary");
    const name = optionString(goal.name);
    summary.append(node("span", "goal-name", name || `Goal ${index + 1}`));
    const identifier = node("span", "goal-id", `#${id}`);
    identifier.title = "Server goal ID";
    summary.append(identifier);
    const body = node("div", "goal-body");
    const hypotheses: unknown[] = Array.isArray(goal.hypotheses) ? goal.hypotheses : [];
    if (hypotheses.length) {
      const context = this.disclosure(`context:${key}`, "hypotheses");
      context.append(node("summary", "hypotheses-summary", `${hypotheses.length} ${hypotheses.length === 1 ? "hypothesis" : "hypotheses"}`));
      const list = node("ul", "hypotheses-list");
      hypotheses.forEach((hypothesis, i) => {
        const item = node("li");
        item.append(this.printer.create(hypothesisPretty(hypothesis), `Hypothesis ${i + 1}`));
        list.append(item);
      });
      context.append(list);
      body.append(context);
    }
    const conclusion = node("div", "conclusion");
    conclusion.append(this.printer.create(goal.goal, "Conclusion"));
    body.append(conclusion);
    details.append(summary, body);
    return details;
  }

  renderMessages(messages: Message[], state: BridgeState): void {
    byId("message-count").textContent = String(messages.length);
    const errors = messages.filter(([severity]) => severity === 1).length;
    const warnings = messages.filter(([severity]) => severity === 2).length;
    const counts: string[] = [];
    if (errors) counts.push(`${errors} ${errors === 1 ? "error" : "errors"}`);
    if (warnings) counts.push(`${warnings} ${warnings === 1 ? "warning" : "warnings"}`);
    byId("message-summary").textContent = counts.join(", ");
    const container = byId("messages-content");
    if (messages.length === 0) {
      container.replaceChildren(node("p", "empty-messages", state.status === "checking" ? "Waiting for messages" : "No messages"));
      return;
    }
    const list = node("ol", "message-list");
    messages.forEach(([severity, message], index) => {
      const [key, label] = LEVELS[severity] || ["message", "Message"];
      const item = node("li", "message");
      item.dataset.level = key;
      const header = node("div", "message-header");
      header.append(node("span", "message-level", label));
      item.append(header, this.printer.create(message, `${label} message ${index + 1}`));
      list.append(item);
    });
    container.replaceChildren(list);
  }
}

type BrowserCommand = { command: Command; uri: string; version: number; source: "browser" };

class ApiError extends Error {
  readonly status: number;

  constructor(message: string, status = 0) {
    super(message);
    this.status = status;
  }
}

function validState(value: unknown): value is BridgeState {
  const state = record(value);
  if (!state) return false;
  const documentInfo = record(state.document);
  const toolchain = record(state.toolchain);
  return typeof state.revision === "number" && Number.isSafeInteger(state.revision) && state.revision >= 0 &&
    typeof state.status === "string" && ["idle", "checking", "ready", "error"].includes(state.status) &&
    (state.proof === null || record(state.proof) !== null) &&
    (state.document === null || (documentInfo !== null && typeof documentInfo.uri === "string" &&
      typeof documentInfo.name === "string" && Number.isSafeInteger(documentInfo.version))) &&
    (state.error === null || typeof state.error === "string") &&
    (state.toolchain === null || (toolchain !== null && typeof toolchain.missing === "boolean" &&
      typeof toolchain.message === "string" && (toolchain.url === null || typeof toolchain.url === "string")));
}

class BridgeTransport {
  readonly token: string;

  constructor(token: string) {
    this.token = token;
  }

  request(path: "/api/state", command: null, signal?: AbortSignal): Promise<BridgeState>;
  request(path: "/api/command", command: BrowserCommand, signal?: AbortSignal): Promise<void>;
  async request(path: string, command: BrowserCommand | null, signal?: AbortSignal): Promise<BridgeState | void> {
    const controller = new AbortController();
    const abort = () => controller.abort();
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) controller.abort();
    let timedOut = false;
    const timeout = setTimeout(() => { timedOut = true; controller.abort(); }, 10000);
    try {
      const headers: Record<string, string> = { Authorization: `Bearer ${this.token}`, Accept: "application/json" };
      if (command) headers["Content-Type"] = "application/json";
      const response = await fetch(path, {
        method: command ? "POST" : "GET",
        headers,
        body: command ? JSON.stringify(command) : undefined,
        signal: controller.signal,
        credentials: "omit",
        mode: "same-origin",
        redirect: "error",
        cache: "no-store",
        referrerPolicy: "no-referrer",
      });
      if (!response.ok) {
        const text = await response.text();
        let detail = text.trim().slice(0, 1500);
        try {
          const data = record(JSON.parse(text) as unknown);
          detail = typeof data?.error === "string" ? data.error : (typeof data?.message === "string" ? data.message : detail);
        } catch { /* A plain-text bridge error is also useful. */ }
        throw new ApiError(detail || `The bridge returned HTTP ${response.status}.`, response.status);
      }
      if (command) {
        if (response.status !== 202) throw new ApiError("The bridge did not acknowledge the command.");
        return;
      }
      const state: unknown = await response.json();
      if (!validState(state)) throw new ApiError("The bridge returned an invalid proof state.");
      return state;
    } catch (error) {
      if (timedOut) throw new ApiError("The local bridge did not respond in time.");
      if (signal?.aborted || error instanceof ApiError) throw error;
      throw new ApiError("Could not read the local bridge. Keep the project open in Zed.");
    } finally {
      clearTimeout(timeout);
      signal?.removeEventListener("abort", abort);
    }
  }

  state(signal: AbortSignal): Promise<BridgeState> { return this.request("/api/state", null, signal); }
  command(command: BrowserCommand): Promise<void> { return this.request("/api/command", command); }
}

class ProofSession {
  readonly transport: BridgeTransport;
  readonly view: ProofView;
  timer: number | undefined;
  pollController: AbortController | null = null;
  commandActive = false;
  suspended = false;
  unauthorized = false;
  failures = 0;
  revision: number | null = null;

  constructor(transport: BridgeTransport, view: ProofView) {
    this.transport = transport;
    this.view = view;
    window.addEventListener("online", () => this.reconnect());
    document.addEventListener("visibilitychange", () => { if (!document.hidden) this.reconnect(); });
    window.addEventListener("pagehide", () => { this.suspended = true; this.cancelPoll(); });
    window.addEventListener("pageshow", (event) => { if (event.persisted) { this.suspended = false; this.reconnect(); } });
  }

  start(): void {
    if (!this.transport.token) {
      this.unauthorized = true;
      this.view.setConnection("missing", "This page needs a private session link. Use Open proof view in Zed to open an authorized page. Do not share the session link.");
      return;
    }
    this.schedule(0);
  }

  cancelPoll(): void {
    clearTimeout(this.timer);
    this.timer = undefined;
    this.pollController?.abort();
    this.pollController = null;
  }

  schedule(delay: number): void {
    if (this.suspended || this.unauthorized || this.commandActive) return;
    clearTimeout(this.timer);
    this.timer = setTimeout(() => this.poll(), delay);
  }

  reconnect(): void {
    if (this.suspended || this.unauthorized || this.commandActive) return;
    this.cancelPoll();
    this.schedule(0);
  }

  async poll(): Promise<void> {
    const controller = new AbortController();
    this.pollController = controller;
    let delay = document.hidden ? 2500 : 650;
    try {
      const state = await this.transport.state(controller.signal);
      if (controller.signal.aborted || this.suspended || this.commandActive) return;
      this.failures = 0;
      this.view.setConnection("connected");
      if (state.revision !== this.revision || this.view.pending) {
        this.revision = state.revision;
        this.view.render(state);
      }
    } catch (cause) {
      if (controller.signal.aborted) return;
      const error = cause instanceof ApiError ? cause : new ApiError("The local bridge request failed.");
      if (error.status === 401 || error.status === 403) {
        this.unauthorized = true;
        this.view.setConnection("unauthorized", "This session link is no longer authorized. Open the proof view again from Zed to get a fresh private link.");
      } else {
        delay = Math.min(10000, 1000 * 2 ** Math.min(this.failures++, 4));
        this.view.setConnection("retrying", `${error.message} Retrying in ${delay / 1000} ${delay === 1000 ? "second" : "seconds"}. Any goals still shown are from the last connection and may be out of date.`);
      }
    } finally {
      if (this.pollController === controller) this.pollController = null;
    }
    if (!controller.signal.aborted) this.schedule(delay);
  }

  async command(command: Command): Promise<void> {
    if (this.commandActive || !this.view.canCommand(command)) return;
    const target = this.view.state?.document;
    if (!target) return;
    this.commandActive = true;
    this.cancelPoll();
    this.view.beginCommand(command);
    try {
      await this.transport.command({ command, uri: target.uri, version: target.version, source: "browser" });
      // The bridge publishes the cleared, revisioned state before 202.
      // Never replay a mutation, even if the response was lost.
      this.revision = null;
    } catch (cause) {
      this.revision = null;
      const error = cause instanceof ApiError ? cause : new ApiError("The local bridge request failed.");
      this.view.commandFailed(`${error.message} The command was not resent. The current proof state will be read again before another command.`);
      if (error.status === 401 || error.status === 403) {
        this.unauthorized = true;
        this.view.setConnection("unauthorized", "This session link is no longer authorized. Open the proof view again from Zed.");
      } else this.view.setConnection("retrying", "Reconnecting to read the current proof state. The last proof view may be out of date.");
    } finally {
      this.commandActive = false;
      this.schedule(0);
    }
  }
}

function sessionToken(): string {
  const fragment = new URLSearchParams(window.location.hash.slice(1));
  if (fragment.has("token")) {
    const token = fragment.get("token") || "";
    // Remove the secret from the visible URL and current history entry.
    history.replaceState(null, "", window.location.pathname + window.location.search);
    try {
      if (token) sessionStorage.setItem(TOKEN_KEY, token);
      else sessionStorage.removeItem(TOKEN_KEY);
    } catch { /* The current page still works if session storage is disabled. */ }
    return token;
  }
  try { return sessionStorage.getItem(TOKEN_KEY) || ""; }
  catch { return ""; }
}

function configureTheme(): void {
  type Theme = "system" | "light" | "dark";
  const button = byId("theme");
  if (!(button instanceof HTMLButtonElement)) throw new Error("The theme control is unavailable.");
  const labels: Record<Theme, string> = { system: "System", light: "Light", dark: "Dark" };
  const nextTheme: Record<Theme, Theme> = { system: "light", light: "dark", dark: "system" };
  let savedTheme = "system";
  try { savedTheme = localStorage.getItem(THEME_KEY) || savedTheme; }
  catch { /* A theme preference is optional. */ }
  const apply = (value: string): Theme => {
    const theme = value === "light" || value === "dark" ? value : "system";
    if (theme === "system") delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = theme;
    button.dataset.theme = theme;
    button.title = `Theme: ${labels[theme]}. Switch to ${labels[nextTheme[theme]]}`;
    button.setAttribute("aria-label", button.title);
    for (const icon of document.querySelectorAll<SVGGElement>("[data-theme-icon]")) {
      icon.toggleAttribute("hidden", icon.dataset.themeIcon !== theme);
    }
    return theme;
  };
  let theme = apply(savedTheme);
  button.addEventListener("click", () => {
    theme = apply(nextTheme[theme]);
    try { localStorage.setItem(THEME_KEY, theme); }
    catch { /* Keep the selected theme for this page. */ }
  });
}

// Read and erase the URL token before starting any network work.
const token = sessionToken();
configureTheme();
let session: ProofSession;
const view = new ProofView({
  onCommand: (command) => session.command(command),
  onReconnect: () => session.reconnect(),
});
session = new ProofSession(new BridgeTransport(token), view);
session.start();
