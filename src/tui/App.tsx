import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Box, render, Text, useApp, useInput, useStdout } from "ink";
import type { ProviderId } from "../types.ts";
import type { TurnEvent } from "../adapters/types.ts";
import { isPermissionMode, PERMISSION_MODES, type PermissionMode } from "../types.ts";
import type { BatonStore, MessageRow, ThreadRow, UsageRow } from "../store.ts";
import type { BatonAdapter } from "../adapters/types.ts";
import { runTurn } from "../runtime.ts";
import {
  describeAuth,
  loadConfigFile,
  resolveConfig,
  saveConfigFile,
  type EffectiveConfig,
} from "../config.ts";
import { cacheHitRate, formatTokenCount, formatUsageLine, sumUsage } from "../usage.ts";
import { diffCheckpointAgainstWorktree } from "../checkpoints.ts";
import { gitBranchLabel } from "../gitInfo.ts";
import { windowMessages, wrappedRows, type ViewportItemBase } from "./viewport.ts";

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

const ALTSCREEN_ENTER = "\x1b[?1049h";
const ALTSCREEN_EXIT = "\x1b[?1049l";
const CURSOR_HIDE = "\x1b[?25l";
const CURSOR_SHOW = "\x1b[?25h";

interface ToolLineState {
  label: string;
  status: "running" | "done" | "failed";
}

interface ViewportMessage extends ViewportItemBase {
  provider: string | null;
  model: string | null;
  usage: UsageRow | null;
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
  const { stdout } = useStdout();
  const [size, setSize] = useState({
    rows: stdout.rows ?? 24,
    cols: stdout.columns ?? 80,
  });
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
  const [permissionMode, setPermissionMode] = useState<PermissionMode>(() =>
    resolveConfig({ home, cwd: props.initialThread.cwd })[props.initialThread.currentProvider].permissionMode,
  );
  const modeTouched = useRef(false);
  const [spinnerFrame, setSpinnerFrame] = useState(0);
  const [exitArmTime, setExitArmTime] = useState<number | null>(null);
  const [branchLabel, setBranchLabel] = useState<string | null>(null);
  const [scrollOffset, setScrollOffset] = useState(0);
  const [configVersion, setConfigVersion] = useState(0);
  const runningRef = useRef(false);
  runningRef.current = running;

  useEffect(() => {
    const onResize = () => {
      setSize({ rows: stdout.rows ?? 24, cols: stdout.columns ?? 80 });
    };
    stdout.on("resize", onResize);
    return () => {
      stdout.off("resize", onResize);
    };
  }, [stdout]);

  useEffect(() => {
    stdout.write(ALTSCREEN_ENTER + CURSOR_HIDE);
    return () => {
      stdout.write(CURSOR_SHOW + ALTSCREEN_EXIT);
    };
  }, [stdout]);

  const config: EffectiveConfig = useMemo(
    () => resolveConfig({ home, cwd: thread.cwd }),
    [home, thread.cwd, configVersion],
  );

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

  useEffect(() => {
    setScrollOffset(0);
  }, [history.length]);

  const usageByTurn = useMemo(() => {
    const map = new Map<number, UsageRow>();
    for (const row of usageRows) {
      if (!map.has(row.turn)) map.set(row.turn, row);
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
      setDiffOutput(null);
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

  const applyProviderDefaults = useCallback(
    (provider: ProviderId) => {
      if (!modeTouched.current) {
        setPermissionMode(config[provider].permissionMode);
      }
    },
    [config],
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
          setNotice(
            SLASH_COMMANDS.map((entry) => `${entry.command.padEnd(14)} ${entry.description}`).join("\n"),
          );
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
          const next = target as ProviderId;
          store.setThreadProvider(thread.id, next, args[1] ?? null);
          applyProviderDefaults(next);
          const effectiveModel = args[1] ?? config[next].model;
          const existing = store.getProviderSession(thread.id, next);
          setNotice(
            existing
              ? `switched to ${next}/${effectiveModel}, resuming native session ${existing.nativeSessionId.slice(0, 8)}`
              : `switched to ${next}/${effectiveModel}. First turn carries a context handoff.`,
          );
          refreshThreadState(thread.id);
          return true;
        }
        case "/model": {
          const requested = args[0];
          if (!requested) {
            const effective = thread.currentModel ?? config[thread.currentProvider].model;
            const source = thread.currentModel ? "thread" : "harness config";
            setNotice(`current model: ${thread.currentProvider}/${effective} (${source})`);
            return true;
          }
          store.setThreadModel(thread.id, requested);
          refreshThreadState(thread.id);
          return true;
        }
        case "/mode": {
          const mode = args[0];
          if (!mode || !isPermissionMode(mode)) {
            setNotice(`usage: /mode <${PERMISSION_MODES.join("|")}> (current: ${permissionMode})`);
            return true;
          }
          modeTouched.current = true;
          setPermissionMode(mode);
          setNotice(`permission mode: ${mode}`);
          return true;
        }
        case "/auth": {
          if (args.length === 0) {
            setNotice(
              `claude: ${describeAuth(config.claude)} | codex: ${describeAuth(config.codex)}`,
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
              `current ${provider}: ${describeAuth(config[provider])}. usage: /auth ${provider} <subscription|api>`,
            );
            return true;
          }
          if (mode === "api" && !config[provider].apiKey) {
            setNotice(
              `no API key for ${provider}. Set ${config[provider].apiKeyEnvName} in the environment, ${provider === "claude" ? "~/.claude/settings.json env" : "~/.codex/config.toml provider"}, or ${home}/config.json.`,
            );
            return true;
          }
          const file = loadConfigFile(home) ?? {};
          file[provider] = { ...(file[provider] ?? {}), mode };
          saveConfigFile(home, file);
          setConfigVersion((version) => version + 1);
          setNotice(`${provider} billing set to ${mode}`);
          return true;
        }
        case "/new": {
          const title = args.join(" ").trim() || "New thread";
          const created = store.createThread({
            title,
            cwd: thread.cwd,
            currentProvider: thread.currentProvider,
            currentModel: null,
          });
          setThread(created);
          setHistory([]);
          setUsageRows([]);
          setBranchLabel(null);
          setDiffOutput(null);
          setScrollOffset(0);
          setNotice(`new thread ${created.id.slice(0, 8)}`);
          return true;
        }
        case "/threads": {
          const threads = store.listThreads(thread.cwd);
          const lines = threads
            .slice(0, 20)
            .map(
              (candidate) =>
                `${candidate.id.slice(0, 8)}  ${candidate.currentProvider}/${candidate.currentModel ?? config[candidate.currentProvider].model}  ${candidate.title}`,
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
          setScrollOffset(0);
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
            setDiffOutput(lines.slice(0, 400).join("\n") + (lines.length > 400 ? "\n[...truncated]" : ""));
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
    [
      applyProviderDefaults,
      config,
      exit,
      home,
      permissionMode,
      refreshThreadState,
      store,
      thread,
      totals,
      usageRows.length,
    ],
  );

  const onSubmit = useCallback(
    async (text: string) => {
      setInput("");
      setScrollOffset(0);
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

  const suggestions =
    input.startsWith("/") && !input.includes(" ")
      ? SLASH_COMMANDS.filter((entry) => entry.command.startsWith(input))
      : [];

  useInput((input, key) => {
    if (key.ctrl && input === "c") {
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
      return;
    }
    if (key.pageUp) {
      setScrollOffset((offset) => Math.min(offset + 10, Math.max(history.length - 1, 0)));
      return;
    }
    if (key.pageDown) {
      setScrollOffset((offset) => Math.max(offset - 10, 0));
    }
  });

  const composerRows = 4 + (suggestions.length > 0 ? Math.min(suggestions.length, 6) + 2 : 0);
  const noticeRows = notice ? Math.max(wrappedRows(notice, size.cols), 1) : 0;
  const approvalRows = approval ? 4 : 0;
  const liveToolRows = Math.min(toolLines.length, 5);
  const liveStreamingCap = Math.max(3, Math.floor(((size.rows - composerRows - 6) * 3) / 5));
  const liveStreamingRows = streamingText.length > 0 ? Math.min(wrappedRows(streamingText, size.cols), liveStreamingCap) : 0;
  const liveRows =
    liveToolRows +
    (toolLines.length > liveToolRows ? 1 : 0) +
    (liveStreamingRows > 0 ? liveStreamingRows + 1 : 0);
  const statusRows = 1;
  const flexRows = Math.max(
    size.rows - statusRows - composerRows - noticeRows - approvalRows - liveRows,
    0,
  );
  const transcriptBudget = Math.max(flexRows - (scrollOffset > 0 ? 1 : 0), 0);

  const viewportMessages = useMemo<ViewportMessage[]>(
    () =>
      history.map((message) => ({
        id: message.id,
        role: message.role,
        text: message.text,
        hasLabel: message.role === "assistant",
        hasUsage: usageByTurn.has(message.turn),
        provider: message.provider,
        model: message.model,
        usage: usageByTurn.get(message.turn) ?? null,
      })),
    [history, usageByTurn],
  );

  const windowed = windowMessages(viewportMessages, {
    budgetRows: transcriptBudget,
    cols: size.cols,
    scrollOffset,
  });

  const authLabel = describeAuth(config[thread.currentProvider]);
  const showSplash = history.length === 0 && !running && streamingText.length === 0 && flexRows >= 12;

  return (
    <Box flexDirection="column" height={size.rows} width={size.cols} overflow="hidden">
      {diffOutput ? (
        <DiffPanel text={diffOutput} rows={flexRows} />
      ) : (
        <Box flexDirection="column" flexGrow={1} overflow="hidden">
          {showSplash ? <Splash version={props.version} /> : null}
          {scrollOffset > 0 ? (
            <Text dimColor>
              ↑ {windowed.hiddenAbove} older messages · PageUp/PageDown scroll
              {windowed.hiddenBelow > 0 ? ` · ${windowed.hiddenBelow} below` : ""}
            </Text>
          ) : null}
          {windowed.visible.map((message) => (
            <MessageView key={message.id} message={message} />
          ))}
        </Box>
      )}
      {liveRows > 0 ? (
        <LiveArea
          toolLines={toolLines.slice(-liveToolRows)}
          toolLinesHidden={Math.max(toolLines.length - liveToolRows, 0)}
          streamingText={streamingText}
          streamingRows={liveStreamingRows}
        />
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
      {notice ? (
        <Box paddingLeft={1} paddingRight={1}>
          <Text color={COLORS.warning} wrap="wrap">
            {notice}
          </Text>
        </Box>
      ) : null}
      {!approval ? (
        <Composer
          running={running}
          value={input}
          onChange={setInput}
          onSubmit={onSubmit}
          onScroll={(delta) =>
            setScrollOffset((offset) => {
              const next = offset + delta;
              return Math.min(Math.max(next, 0), Math.max(history.length - 1, 0));
            })
          }
          provider={thread.currentProvider}
          model={thread.currentModel ?? config[thread.currentProvider].model}
          authLabel={authLabel}
          permissionMode={permissionMode}
          suggestions={suggestions}
        />
      ) : null}
      <StatusBar
        running={running}
        spinnerFrame={SPINNER_FRAMES[spinnerFrame] ?? "⠋"}
        cwd={thread.cwd}
        branch={branchLabel}
        title={thread.title}
        cacheRate={cacheRate}
        totalInput={totals.inputTokens + totals.cacheReadTokens + totals.cacheCreationTokens}
        cols={size.cols}
      />
    </Box>
  );
}

function Splash(props: { version: string }): React.ReactElement {
  return (
    <Box flexDirection="column" alignItems="center" paddingX={2} marginTop={1}>
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

function MessageView(props: { message: ViewportMessage }): React.ReactElement {
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
  const usageLine = message.usage ? formatUsageLine(sumUsage([message.usage])) : null;
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

function LiveArea(props: {
  toolLines: ReadonlyArray<ToolLineState>;
  toolLinesHidden: number;
  streamingText: string;
  streamingRows: number;
}): React.ReactElement {
  const streamingLines = props.streamingText.length > 0 ? props.streamingText.split("\n") : [];
  const visibleStreaming =
    props.streamingRows > 0 ? streamingLines.slice(-props.streamingRows * 2) : [];
  const streamingTruncated = visibleStreaming.length < streamingLines.length;
  return (
    <Box flexDirection="column" paddingLeft={1} paddingRight={1}>
      {props.toolLinesHidden > 0 ? (
        <Text dimColor>+{props.toolLinesHidden} earlier tool calls</Text>
      ) : null}
      {props.toolLines.map((line, index) => (
        <ToolLineView key={index} line={line} />
      ))}
      {props.streamingRows > 0 ? (
        <>
          {streamingTruncated ? <Text dimColor>…streaming</Text> : null}
          <Text wrap="wrap">{visibleStreaming.join("\n")}</Text>
        </>
      ) : null}
    </Box>
  );
}

function ToolLineView(props: { line: ToolLineState }): React.ReactElement {
  const color =
    props.line.status === "running"
      ? COLORS.accent
      : props.line.status === "failed"
        ? COLORS.error
        : COLORS.success;
  const marker = props.line.status === "running" ? "◐" : props.line.status === "failed" ? "✗" : "✓";
  return (
    <Text color={color} wrap="wrap">
      {marker} {props.line.label}
    </Text>
  );
}

function DiffPanel(props: { text: string; rows: number }): React.ReactElement {
  const lines = props.text.split("\n");
  const budget = Math.max(props.rows - 3, 1);
  const visible = lines.slice(0, budget);
  return (
    <Box flexDirection="column" flexGrow={1} overflow="hidden" borderStyle="round" borderColor={COLORS.border} paddingX={1}>
      <Text dimColor>diff · /clear to close</Text>
      {visible.map((line, index) => (
        <Text
          key={index}
          color={line.startsWith("+") ? COLORS.success : line.startsWith("-") ? COLORS.error : undefined}
          wrap="truncate"
        >
          {line}
        </Text>
      ))}
      {lines.length > budget ? <Text dimColor>[...{lines.length - budget} more lines]</Text> : null}
    </Box>
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
    <Box flexDirection="column" borderStyle="round" borderColor={COLORS.warning} paddingX={1}>
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
  onScroll: (delta: number) => void;
  provider: ProviderId;
  model: string;
  authLabel: string;
  permissionMode: PermissionMode;
  suggestions: ReadonlyArray<{ command: string; description: string }>;
}): React.ReactElement {
  const [selectedIndex, setSelectedIndex] = useState(0);
  const suggestions = props.suggestions;
  const clampedIndex = Math.min(selectedIndex, Math.max(suggestions.length - 1, 0));

  useEffect(() => {
    setSelectedIndex(0);
  }, [props.value]);

  const acceptSuggestion = useCallback(
    (index: number) => {
      const entry = suggestions[index];
      if (!entry) return;
      props.onChange(`${entry.command} `);
    },
    [props, suggestions],
  );

  useInput((input, key) => {
    if (suggestions.length > 0) {
      if (key.upArrow) {
        setSelectedIndex((index) => {
          const current = Math.min(index, suggestions.length - 1);
          return (current - 1 + suggestions.length) % suggestions.length;
        });
        return;
      }
      if (key.downArrow) {
        setSelectedIndex((index) => {
          const current = Math.min(index, suggestions.length - 1);
          return (current + 1) % suggestions.length;
        });
        return;
      }
      if (key.tab) {
        acceptSuggestion(clampedIndex);
        return;
      }
      if (key.return) {
        const exact = suggestions.find((entry) => entry.command === props.value.trim());
        if (exact) {
          void props.onSubmit(props.value);
        } else {
          acceptSuggestion(clampedIndex);
        }
        return;
      }
    } else {
      if (key.upArrow) {
        props.onScroll(1);
        return;
      }
      if (key.downArrow) {
        props.onScroll(-1);
        return;
      }
      if (key.return) {
        void props.onSubmit(props.value);
        return;
      }
    }
    if (key.backspace || key.delete) {
      props.onChange(props.value.slice(0, -1));
      return;
    }
    if (key.ctrl || key.meta || key.escape || key.tab || key.return || key.upArrow || key.downArrow) return;
    if (input.length > 0) props.onChange(props.value + input);
  });

  const maxVisible = 6;
  const windowStart = Math.max(
    Math.min(clampedIndex - (maxVisible - 1), suggestions.length - maxVisible),
    0,
  );
  const visibleSuggestions = suggestions.slice(windowStart, windowStart + maxVisible);

  return (
    <Box flexDirection="column" marginTop={1}>
      {suggestions.length > 0 ? (
        <Box flexDirection="column" borderStyle="round" borderColor={COLORS.border} paddingX={1}>
          {visibleSuggestions.map((entry, index) => (
            <Text key={entry.command} color={windowStart + index === clampedIndex ? COLORS.accent : COLORS.muted}>
              {windowStart + index === clampedIndex ? "❯ " : "  "}
              {entry.command.padEnd(12)} {entry.description}
            </Text>
          ))}
          <Text dimColor>
            ↑↓ select (wraps) · tab/enter complete
            {suggestions.length > maxVisible ? ` · ${suggestions.length} matches` : ""}
          </Text>
        </Box>
      ) : null}
      <Box
        borderStyle="round"
        borderColor={props.running ? COLORS.border : COLORS.accent}
        paddingLeft={1}
        paddingRight={1}
      >
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
  cols: number;
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
  const rightLabel = `${cacheBadge} · ${formatTokenCount(props.totalInput)} in`;
  const maxTitle = Math.max(props.cols - location.length - rightLabel.length - 8, 4);
  const title = props.title.length > maxTitle ? `${props.title.slice(0, maxTitle)}…` : props.title;
  return (
    <Box justifyContent="space-between" paddingLeft={1} paddingRight={1}>
      <Text dimColor>
        {props.running ? (
          <Text color={COLORS.accent}>{props.spinnerFrame} working · ctrl+c to interrupt</Text>
        ) : (
          `${location} · ${title}`
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
