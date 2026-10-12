import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Box, render, Static, Text, useApp, useInput } from "ink";
import type { ProviderId } from "../types.ts";
import type { TurnEvent } from "../adapters/types.ts";
import { isPermissionMode, PERMISSION_MODES, type PermissionMode } from "../types.ts";
import type { BatonStore, MessageRow, ThreadRow, UsageRow } from "../store.ts";
import type { BatonAdapter } from "../adapters/types.ts";
import { runTurn } from "../runtime.ts";
import { loadConfig, describeAuth, saveConfig } from "../config.ts";
import { cacheHitRate, formatTokenCount, formatUsageLine, sumUsage } from "../usage.ts";
import { diffCheckpointAgainstWorktree } from "../checkpoints.ts";
import { gitBranchLabel } from "../gitInfo.ts";

const COLORS = {
  accent: "#f0c674",
  secondary: "#c4b5fd",
  text: "#e5e7eb",
  muted: "#94a3b8",
  border: "#3b4450",
  userBg: "#30363d",
  success: "#86efac",
  error: "#fca5a5",
  warning: "#fbbf24",
} as const;

const SPINNER_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const LOGO_ROWS = [
  "██████╗  █████╗ █████████╗  ██████╗ ███╗   ██╗",
  "██╔══██╗██╔══██╗╚══██╔══╝ ██╔═══██╗████╗  ██║",
  "██████╔╝███████║   ██║    ██║   ██║██╔██╗ ██║",
  "██╔══██╗██╔══██║   ██║    ██║   ██║██║╚██╗██║",
  "██████╔╝██║  ██║   ██║    ╚██████╔╝██║ ╚████║",
  "╚═════╝ ╚═╝  ╚═╝   ╚═╝     ╚═════╝ ╚═╝  ╚═══╝",
];

const SLASH_COMMANDS: ReadonlyArray<{ command: string; description: string }> = [
  { command: "/switch", description: "switch harness: /switch <claude|codex> [model]" },
  { command: "/model", description: "set model for this thread" },
  { command: "/mode", description: "permission mode: default|acceptEdits|bypassPermissions|plan" },
  { command: "/auth", description: "billing mode: /auth <provider> <subscription|api>" },
  { command: "/new", description: "start a new thread" },
  { command: "/threads", description: "list threads in this directory" },
  { command: "/open", description: "open a thread by id prefix" },
  { command: "/usage", description: "token usage and cache hit rate" },
  { command: "/checkpoints", description: "list checkpoints for this thread" },
  { command: "/diff", description: "diff a checkpoint against the working tree" },
  { command: "/clear", description: "clear panels and notices" },
  { command: "/quit", description: "exit baton" },
];

interface ToolLineState {
  label: string;
  status: "running" | "done" | "failed";
}

interface BatonAppProps {
  store: BatonStore;
  adapters: Record<ProviderId, BatonAdapter>;
  home: string;
  initialThread: ThreadRow;
  initialMessages: MessageRow[];
  initialUsage: UsageRow[];
  version: string;
}

export function BatonApp(props: BatonAppProps): React.ReactElement {
  const { store, adapters, home } = props;
  const { exit } = useApp();
  const [thread, setThread] = useState<ThreadRow>(props.initialThread);
  const [history, setHistory] = useState<MessageRow[]>(props.initialMessages);
  const [usageRows, setUsageRows] = useState<UsageRow[]>(props.initialUsage);
  const [input, setInput] = useState("");
  const [running, setRunning] = useState(false);
  const [streamingText, setStreamingText] = useState("");
  const [toolLines, setToolLines] = useState<ToolLineState[]>([]);
  const [approval, setApproval] = useState<Extract<TurnEvent, { type: "approval-request" }> | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [diffOutput, setDiffOutput] = useState<string | null>(null);
  const [permissionMode, setPermissionMode] = useState<PermissionMode>("default");
  const [spinnerFrame, setSpinnerFrame] = useState(0);
  const [exitArmTime, setExitArmTime] = useState<number | null>(null);
  const [branchLabel, setBranchLabel] = useState<string | null>(null);
  const runningRef = useRef(false);
  runningRef.current = running;

  useEffect(() => {
    void gitBranchLabel(thread.cwd).then(setBranchLabel);
  }, [thread.cwd]);

  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => {
      setSpinnerFrame((frame) => (frame + 1) % SPINNER_FRAMES.length);
    }, 80);
    return () => clearInterval(timer);
  }, [running]);

  const usageByTurn = useMemo(() => {
    const map = new Map<number, UsageRow>();
    for (const row of usageRows) {
      const existing = map.get(row.turn);
      if (!existing) map.set(row.turn, row);
    }
    return map;
  }, [usageRows]);

  const totals = useMemo(() => sumUsage(usageRows), [usageRows]);
  const cacheRate = useMemo(() => cacheHitRate(totals), [totals]);

  const refreshThreadState = useCallback(
    (threadId: string) => {
      const fresh = store.getThread(threadId);
      if (fresh) setThread(fresh);
      setHistory(store.listMessages(threadId));
      setUsageRows(store.listUsage(threadId));
    },
    [store],
  );

  const emit = useCallback((event: TurnEvent) => {
    if (event.type === "text-delta") {
      setStreamingText((current) => current + event.text);
    } else if (event.type === "tool-started") {
      setToolLines((current) => [
        ...current,
        { label: `${event.tool}${event.detail ? ` ${event.detail}` : ""}`, status: "running" },
      ]);
    } else if (event.type === "tool-completed") {
      setToolLines((current) => {
        const next = [...current];
        for (let index = next.length - 1; index >= 0; index--) {
          const line = next[index];
          if (line && line.status === "running") {
            line.status = event.exitCode !== null && event.exitCode !== 0 ? "failed" : "done";
            break;
          }
        }
        return next;
      });
    } else if (event.type === "approval-request") {
      setApproval(event);
    }
  }, []);

  const submitTurn = useCallback(
    async (text: string) => {
      if (runningRef.current) return;
      setRunning(true);
      setStreamingText("");
      setToolLines([]);
      setNotice(null);
      const current = store.getThread(thread.id) ?? thread;
      const outcome = await runTurn({ store, adapters, home }, current, text, emit, { permissionMode });
      setRunning(false);
      setStreamingText("");
      setToolLines([]);
      refreshThreadState(thread.id);
      if (!outcome.ok && outcome.error) {
        setNotice(`turn failed: ${outcome.error}`);
      }
    },
    [adapters, emit, home, permissionMode, refreshThreadState, store, thread],
  );

  const handleCommand = useCallback(
    async (raw: string): Promise<boolean> => {
      const trimmed = raw.trim();
      if (!trimmed.startsWith("/")) return false;
      const parts = trimmed.split(/\s+/);
      const command = parts[0];
      const args = parts.slice(1);
      switch (command) {
        case "/help":
          setNotice(SLASH_COMMANDS.map((entry) => `${entry.command.padEnd(14)} ${entry.description}`).join("\n"));
          return true;
        case "/quit":
        case "/exit":
          exit();
          return true;
        case "/clear":
          setDiffOutput(null);
          setNotice(null);
          return true;
        case "/switch": {
          const target = args[0];
          if (target !== "claude" && target !== "codex") {
            setNotice("usage: /switch <claude|codex> [model]");
            return true;
          }
          const config = loadConfig(home);
          const model = args[1] ?? config.defaultModels[target];
          const next = target as ProviderId;
          store.setThreadProvider(thread.id, next, model);
          const existing = store.getProviderSession(thread.id, next);
          setNotice(
            existing
              ? `switched to ${next}/${model}, resuming native session ${existing.nativeSessionId.slice(0, 8)}`
              : `switched to ${next}/${model}. First turn carries a context handoff.`,
          );
          refreshThreadState(thread.id);
          return true;
        }
        case "/model": {
          const model = args[0];
          if (!model) {
            setNotice(`current model: ${thread.currentProvider}/${thread.currentModel}`);
            return true;
          }
          store.setThreadModel(thread.id, model);
          refreshThreadState(thread.id);
          return true;
        }
        case "/mode": {
          const mode = args[0];
          if (!mode || !isPermissionMode(mode)) {
            setNotice(`usage: /mode <${PERMISSION_MODES.join("|")}> (current: ${permissionMode})`);
            return true;
          }
          setPermissionMode(mode);
          setNotice(`permission mode: ${mode}`);
          return true;
        }
        case "/auth": {
          const config = loadConfig(home);
          if (args.length === 0) {
            setNotice(
              `claude: ${describeAuth(config.claude, "claude")} | codex: ${describeAuth(config.codex, "codex")}`,
            );
            return true;
          }
          const provider = args[0] as ProviderId;
          if (provider !== "claude" && provider !== "codex") {
            setNotice("usage: /auth <claude|codex> <subscription|api>");
            return true;
          }
          const mode = args[1];
          if (mode !== "subscription" && mode !== "api") {
            setNotice(
              `current ${provider}: ${describeAuth(config[provider], provider)}. usage: /auth ${provider} <subscription|api>`,
            );
            return true;
          }
          if (mode === "api" && !config[provider].apiKey) {
            setNotice(
              `no API key configured for ${provider}. Set ${provider === "claude" ? "ANTHROPIC_API_KEY" : "OPENAI_API_KEY"} in the environment or config.json first.`,
            );
            return true;
          }
          config[provider].mode = mode;
          saveConfig(home, config);
          setNotice(`${provider} billing: ${describeAuth(config[provider], provider)}`);
          return true;
        }
        case "/new": {
          const config = loadConfig(home);
          const title = args.join(" ").trim() || "New thread";
          const created = store.createThread({
            title,
            cwd: thread.cwd,
            currentProvider: thread.currentProvider,
            currentModel: config.defaultModels[thread.currentProvider],
          });
          setThread(created);
          setHistory([]);
          setUsageRows([]);
          setBranchLabel(null);
          setDiffOutput(null);
          setNotice(`new thread ${created.id.slice(0, 8)}`);
          return true;
        }
        case "/threads": {
          const threads = store.listThreads(thread.cwd);
          const lines = threads
            .slice(0, 20)
            .map(
              (candidate) =>
                `${candidate.id.slice(0, 8)}  ${candidate.currentProvider}/${candidate.currentModel}  ${candidate.title}`,
            );
          setNotice(lines.join("\n") || "no threads in this directory");
          return true;
        }
        case "/open": {
          const id = args[0];
          if (!id) {
            setNotice("usage: /open <threadId>");
            return true;
          }
          const target = store.listThreads().find((candidate) => candidate.id.startsWith(id));
          if (!target) {
            setNotice(`no thread matches "${id}"`);
            return true;
          }
          setThread(target);
          setHistory(store.listMessages(target.id));
          setUsageRows(store.listUsage(target.id));
          setDiffOutput(null);
          setNotice(`opened ${target.title}`);
          return true;
        }
        case "/usage": {
          setNotice(usageRows.length === 0 ? "no usage yet" : formatUsageLine(totals));
          return true;
        }
        case "/checkpoints": {
          const checkpoints = store.listCheckpoints(thread.id);
          setNotice(
            checkpoints.length === 0
              ? "no checkpoints yet"
              : checkpoints.map((checkpoint) => `turn ${checkpoint.turn}  ${checkpoint.gitRef}`).join("\n"),
          );
          return true;
        }
        case "/diff": {
          const checkpoints = store.listCheckpoints(thread.id);
          if (checkpoints.length === 0) {
            setNotice("no checkpoints yet");
            return true;
          }
          const turn = args[0] !== undefined ? Number(args[0]) : checkpoints[checkpoints.length - 1]?.turn;
          if (turn === undefined || !checkpoints.some((checkpoint) => checkpoint.turn === turn)) {
            setNotice(`no checkpoint for turn ${String(args[0] ?? "")}`);
            return true;
          }
          const checkpoint = checkpoints.find((candidate) => candidate.turn === turn);
          if (!checkpoint) {
            setNotice("checkpoint not found");
            return true;
          }
          try {
            const diff = await diffCheckpointAgainstWorktree(thread.cwd, checkpoint.gitRef);
            const lines = diff.split("\n");
            setDiffOutput(lines.slice(0, 200).join("\n") + (lines.length > 200 ? "\n[...truncated]" : ""));
          } catch (error) {
            setNotice(`diff failed: ${error instanceof Error ? error.message : String(error)}`);
          }
          return true;
        }
        default:
          setNotice(`unknown command ${command}. Try /help.`);
          return true;
      }
    },
    [exit, home, permissionMode, refreshThreadState, store, thread, totals, usageRows.length],
  );

  const onSubmit = useCallback(
    async (text: string) => {
      setInput("");
      if (text.trim().length === 0) return;
      if (await handleCommand(text)) return;
      void submitTurn(text.trim());
    },
    [handleCommand, submitTurn],
  );

  const interrupt = useCallback(() => {
    const adapter = adapters[thread.currentProvider];
    void adapter?.interrupt();
    setNotice("interrupting...");
  }, [adapters, thread.currentProvider]);

  useInput((input, key) => {
    if (!key.ctrl || input !== "c") return;
    if (runningRef.current) {
      interrupt();
      return;
    }
    const now = Date.now();
    if (exitArmTime !== null && now - exitArmTime < 2000) {
      exit();
      return;
    }
    setExitArmTime(now);
    setNotice("press ctrl+c again to exit");
  });

  const config = useMemo(() => loadConfig(home), [home]);
  const authLabel = describeAuth(config[thread.currentProvider], thread.currentProvider);
  const suggestions =
    input.startsWith("/") && !input.includes(" ")
      ? SLASH_COMMANDS.filter((entry) => entry.command.startsWith(input))
      : [];

  return (
    <Box flexDirection="column">
      {history.length === 0 && !running && streamingText.length === 0 ? <Splash version={props.version} /> : null}
      <Static items={history}>
        {(message) => (
          <MessageView key={message.id} message={message} usage={usageByTurn.get(message.turn) ?? null} />
        )}
      </Static>
      {toolLines.length > 0 || streamingText.length > 0 ? (
        <Box flexDirection="column" paddingLeft={1} paddingRight={1}>
          {toolLines.map((line, index) => (
            <ToolLineView key={index} line={line} />
          ))}
          {streamingText.length > 0 ? <Text wrap="wrap">{streamingText}</Text> : null}
        </Box>
      ) : null}
      {approval ? (
        <ApprovalBox
          summary={approval.summary}
          onDecide={(decision) => {
            setApproval(null);
            void approval.respond(decision);
          }}
        />
      ) : null}
      {diffOutput ? (
        <Box flexDirection="column" borderStyle="round" borderColor={COLORS.border} paddingX={1} marginTop={1}>
          <Text dimColor>diff · /clear to hide</Text>
          <Text wrap="wrap">{diffOutput}</Text>
        </Box>
      ) : null}
      {notice ? (
        <Box paddingLeft={1} paddingRight={1} marginTop={1}>
          <Text color={COLORS.warning} wrap="wrap">
            {notice}
          </Text>
        </Box>
      ) : null}
      <Composer
        running={running}
        value={input}
        onChange={setInput}
        onSubmit={onSubmit}
        provider={thread.currentProvider}
        model={thread.currentModel}
        authLabel={authLabel}
        permissionMode={permissionMode}
        suggestions={suggestions}
      />
      <StatusBar
        running={running}
        spinnerFrame={SPINNER_FRAMES[spinnerFrame] ?? "⠋"}
        cwd={thread.cwd}
        branch={branchLabel}
        title={thread.title}
        cacheRate={cacheRate}
        totalInput={totals.inputTokens + totals.cacheReadTokens + totals.cacheCreationTokens}
      />
    </Box>
  );
}

function Splash(props: { version: string }): React.ReactElement {
  return (
    <Box flexDirection="column" alignItems="center" paddingX={2} paddingY={1} marginTop={1}>
      {LOGO_ROWS.map((row, index) => (
        <Text key={index} color={COLORS.accent}>
          {row}
        </Text>
      ))}
      <Text dimColor>one thread · every harness · pass the baton</Text>
      <Text dimColor>{props.version} · type a message, or /help for commands</Text>
    </Box>
  );
}

function MessageView(props: { message: MessageRow; usage: UsageRow | null }): React.ReactElement {
  const { message } = props;
  if (message.role === "user") {
    return (
      <Box flexDirection="column" marginTop={1}>
        <Box backgroundColor={COLORS.userBg} paddingLeft={1} paddingRight={1}>
          <Text wrap="wrap">{message.text}</Text>
        </Box>
      </Box>
    );
  }
  const usageLine = props.usage ? formatUsageLine(sumUsage([props.usage])) : null;
  return (
    <Box flexDirection="column" marginTop={1}>
      <Text dimColor>
        {message.provider ?? "?"} · {message.model ?? "?"}
      </Text>
      <Box paddingLeft={1}>
        <Text wrap="wrap">{message.text}</Text>
      </Box>
      {usageLine ? (
        <Box paddingLeft={1}>
          <Text dimColor>{usageLine}</Text>
        </Box>
      ) : null}
    </Box>
  );
}

function ToolLineView(props: { line: ToolLineState }): React.ReactElement {
  const color = props.line.status === "running" ? COLORS.accent : props.line.status === "failed" ? COLORS.error : COLORS.success;
  const marker = props.line.status === "running" ? "◐" : props.line.status === "failed" ? "✗" : "✓";
  return (
    <Text color={color}>
      {marker} {props.line.label}
    </Text>
  );
}

function ApprovalBox(props: {
  summary: string;
  onDecide: (decision: "accept" | "acceptAlways" | "deny") => void;
}): React.ReactElement {
  useInput((input) => {
    if (input === "y") props.onDecide("accept");
    else if (input === "a") props.onDecide("acceptAlways");
    else if (input === "n") props.onDecide("deny");
  });
  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      borderColor={COLORS.warning}
      paddingX={1}
      marginTop={1}
    >
      <Text color={COLORS.warning} wrap="wrap">
        approval needed: {props.summary}
      </Text>
      <Text dimColor>[y] allow once · [a] allow for session · [n] deny</Text>
    </Box>
  );
}

function Composer(props: {
  running: boolean;
  value: string;
  onChange: (value: string) => void;
  onSubmit: (value: string) => void | Promise<void>;
  provider: ProviderId;
  model: string;
  authLabel: string;
  permissionMode: PermissionMode;
  suggestions: ReadonlyArray<{ command: string; description: string }>;
}): React.ReactElement {
  useInput((input, key) => {
    if (key.return) {
      void props.onSubmit(props.value);
      return;
    }
    if (key.backspace || key.delete) {
      props.onChange(props.value.slice(0, -1));
      return;
    }
    if (key.ctrl || key.meta || key.escape || key.tab || key.return) return;
    if (input.length > 0) props.onChange(props.value + input);
  });
  return (
    <Box flexDirection="column" marginTop={1}>
      {props.suggestions.length > 0 ? (
        <Box flexDirection="column" borderStyle="round" borderColor={COLORS.border} paddingX={1}>
          {props.suggestions.slice(0, 6).map((entry, index) => (
            <Text key={entry.command} color={index === 0 ? COLORS.accent : COLORS.muted}>
              {index === 0 ? "❯ " : "  "}
              {entry.command.padEnd(12)} {entry.description}
            </Text>
          ))}
        </Box>
      ) : null}
      <Box borderStyle="round" borderColor={props.running ? COLORS.border : COLORS.accent} paddingLeft={1} paddingRight={1}>
        <Box flexDirection="column">
          <Text color={props.value.length > 0 ? COLORS.text : COLORS.muted} wrap="wrap">
            {props.running ? "" : props.value.length > 0 ? props.value : "Ask anything..."}
            {props.running ? null : <Text inverse> </Text>}
          </Text>
          <Text dimColor>
            <Text color={COLORS.secondary}>{props.provider}</Text> · {props.model} · {props.authLabel} · mode{" "}
            {props.permissionMode}
          </Text>
        </Box>
      </Box>
    </Box>
  );
}

function StatusBar(props: {
  running: boolean;
  spinnerFrame: string;
  cwd: string;
  branch: string | null;
  title: string;
  cacheRate: number | null;
  totalInput: number;
}): React.ReactElement {
  const cacheBadge = props.cacheRate === null ? "cache n/a" : `cache ${(props.cacheRate * 100).toFixed(0)}%`;
  const cacheColor =
    props.cacheRate === null
      ? COLORS.muted
      : props.cacheRate >= 0.7
        ? COLORS.success
        : props.cacheRate >= 0.4
          ? COLORS.warning
          : COLORS.error;
  const homeLabel = props.cwd.replace(process.env.HOME ?? "", "~");
  const location = props.branch ? `${homeLabel}:${props.branch}` : homeLabel;
  return (
    <Box justifyContent="space-between" paddingLeft={1} paddingRight={1}>
      <Text dimColor>
        {props.running ? (
          <Text color={COLORS.accent}>
            {props.spinnerFrame} working · ctrl+c to interrupt
          </Text>
        ) : (
          `${location} · ${props.title}`
        )}
      </Text>
      <Text dimColor>
        <Text color={cacheColor}>{cacheBadge}</Text>
        {` · ${formatTokenCount(props.totalInput)} in`}
      </Text>
    </Box>
  );
}

export interface ChatTuiDeps {
  store: BatonStore;
  adapters: Record<ProviderId, BatonAdapter>;
  home: string;
  thread: ThreadRow;
  version: string;
}

export async function runChatTui(deps: ChatTuiDeps): Promise<void> {
  const messages = deps.store.listMessages(deps.thread.id);
  const usageRows = deps.store.listUsage(deps.thread.id);
  const instance = render(
    <BatonApp
      store={deps.store}
      adapters={deps.adapters}
      home={deps.home}
      initialThread={deps.thread}
      initialMessages={messages}
      initialUsage={usageRows}
      version={deps.version}
    />,
    { exitOnCtrlC: false },
  );
  await instance.waitUntilExit();
}
