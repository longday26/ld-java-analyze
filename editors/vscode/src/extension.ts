import { Dirent, existsSync, promises as fs } from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;
let clientReady: Promise<void> | undefined;
let output: vscode.OutputChannel | undefined;

/** Persistent, inspectable feedback. Transient notifications are easy to miss. */
function writeOutput(title: string, lines: string[]): void {
  if (!output) {
    return;
  }
  output.appendLine(`[${new Date().toLocaleTimeString()}] ${title}`);
  for (const line of lines) {
    output.appendLine(`  ${line}`);
  }
  output.show(true);
}

function report(
  title: string,
  lines: string[],
  level: 'info' | 'warn' | 'error' = 'info',
  notify = true,
): void {
  writeOutput(title, lines);
  if (!notify) {
    return;
  }
  const summary = lines[0] ? `${title}：${lines[0]}` : title;
  const message = `ld-java-analyze: ${summary}`;
  if (level === 'error') {
    vscode.window.showErrorMessage(message);
  } else if (level === 'warn') {
    vscode.window.showWarningMessage(message);
  } else {
    vscode.window.showInformationMessage(message);
  }
}

interface LspPosition {
  line: number;
  character: number;
}

interface LspRange {
  start: LspPosition;
  end: LspPosition;
}

interface ReferenceHit {
  uri: string;
  range: LspRange;
  role: string;
}

interface JarHit {
  label: string;
  uri: string;
  line: number;
  jar: string;
  kind: string;
  hasSource: boolean;
}

interface RootItem extends vscode.QuickPickItem {
  roots?: string[];
  browse?: boolean;
}

interface ResultItem extends vscode.QuickPickItem {
  location?: vscode.Location;
  peek?: boolean;
}

function serverPath(context: vscode.ExtensionContext): string {
  const configured = vscode.workspace
    .getConfiguration('ld-java-analyze')
    .get<string>('serverPath');
  if (configured && configured.trim().length > 0) {
    return configured.trim();
  }
  const binary = process.platform === 'win32' ? 'ld-java-analyze.exe' : 'ld-java-analyze';
  // Installed from a .vsix: the Rust server ships inside the extension.
  const bundled = path.join(context.extensionPath, 'server', binary);
  if (existsSync(bundled)) {
    return bundled;
  }
  // Development mode: extension lives at editors/vscode under the Rust workspace.
  return path.resolve(context.extensionPath, '..', '..', 'target', 'debug', binary);
}

function activePosition():
  | { uri: string; line: number; character: number }
  | undefined {
  const editor = vscode.window.activeTextEditor;
  if (!editor) {
    vscode.window.showWarningMessage('ld-java-analyze: 请先打开 Java / XML 文件');
    return undefined;
  }
  const active = editor.selection.active;
  return {
    uri: editor.document.uri.toString(),
    line: active.line,
    character: active.character,
  };
}

const SKIPPED_DIRS = new Set([
  '.git', '.ld-java-analyze', '.idea', '.vscode', '.gradle', '.mvn',
  'target', 'build', 'out', 'node_modules', 'dist',
]);

/** Workspace directories up to two levels deep, for quick picking. */
async function collectDirectories(root: string, maxDepth: number): Promise<string[]> {
  const out: string[] = [];
  async function walk(dir: string, depth: number): Promise<void> {
    if (depth > maxDepth || out.length >= 40) {
      return;
    }
    let entries: Dirent[];
    try {
      entries = await fs.readdir(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      if (!entry.isDirectory() || entry.name.startsWith('.') || SKIPPED_DIRS.has(entry.name)) {
        continue;
      }
      const full = path.join(dir, entry.name);
      out.push(full);
      await walk(full, depth + 1);
    }
  }
  await walk(root, 1);
  return out;
}

/**
 * QuickPick instead of a native folder dialog: the dialog was easy to miss and
 * cancelling it made the context-menu command look like it did nothing.
 * Returns `undefined` on cancel, `[]` for "whole workspace".
 */
async function pickRoots(currentUri?: string): Promise<string[] | undefined> {
  const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  const items: RootItem[] = [
    {
      label: '$(globe) 整个工作区',
      description: '不过滤，列出全部引用',
      roots: [],
    },
  ];
  if (currentUri) {
    const dir = path.dirname(vscode.Uri.parse(currentUri).fsPath);
    items.push({
      label: '$(file) 当前文件所在目录',
      description: dir,
      roots: [dir],
    });
  }
  if (root) {
    for (const dir of await collectDirectories(root, 2)) {
      items.push({
        label: `$(folder) ${path.relative(root, dir)}`,
        description: dir,
        roots: [dir],
      });
    }
  }
  items.push({ label: '$(folder-opened) 浏览其它目录…', browse: true });

  const picked = await vscode.window.showQuickPick(items, {
    canPickMany: true,
    placeHolder: '选择引用搜索目录（可多选；选「整个工作区」则不过滤）',
  });
  if (!picked || picked.length === 0) {
    vscode.window.showInformationMessage('ld-java-analyze: 已取消目录选择');
    return undefined;
  }
  // Selecting the no-filter entry deliberately means "all references".
  if (picked.some((item) => item.roots?.length === 0 && !item.browse)) {
    return [];
  }
  const roots = new Set<string>();
  for (const item of picked) {
    if (item.browse) {
      const dialog = await vscode.window.showOpenDialog({
        canSelectFiles: false,
        canSelectFolders: true,
        canSelectMany: true,
        openLabel: '选择引用搜索目录',
      });
      for (const uri of dialog ?? []) {
        roots.add(uri.fsPath);
      }
    } else {
      for (const value of item.roots ?? []) {
        roots.add(value);
      }
    }
  }
  return [...roots];
}

function ignoreFilePath(): string | undefined {
  const root = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  return root ? path.join(root, '.ld-java-analyze', 'ignore') : undefined;
}

async function ensureIgnoreFile(): Promise<string | undefined> {
  const file = ignoreFilePath();
  if (!file) {
    vscode.window.showWarningMessage('ld-java-analyze: 请先打开一个工作区');
    return undefined;
  }
  try {
    await fs.access(file);
  } catch {
    await fs.mkdir(path.dirname(file), { recursive: true });
    await fs.writeFile(
      file,
      [
        '# ld-java-analyze 扫描排除规则（按目录名 / 文件名匹配）',
        '# generated/            排除名为 generated 的目录',
        '# dir:build*           排除名字以 build 开头的目录',
        '# file:*.generated.java 排除匹配的文件',
        '# *Test.java           同时匹配目录名和文件名',
        '# !KeepTest.java       反选：最后一条匹配的规则生效',
        '',
      ].join('\n'),
      'utf8',
    );
  }
  return file;
}

async function call<T>(command: string, argument: unknown): Promise<T | undefined> {
  if (clientReady) {
    try {
      await clientReady;
    } catch {
      // Start failure is already reported by activate().
    }
  }
  if (!client) {
    report(`${command} 未执行`, ['语言服务未就绪，请稍候或执行 Developer: Reload Window'], 'error');
    return undefined;
  }
  try {
    const result = await client.sendRequest<T | null>('workspace/executeCommand', {
      command,
      arguments: [argument],
    });
    if (result === null || result === undefined) {
      report(
        `${command} 无返回`,
        ['服务端未返回结果，请查看「ld-java-analyze」输出与 <workspace>/.ld-java-analyze/logs'],
        'error',
      );
      return undefined;
    }
    return result;
  } catch (error) {
    report(`${command} 失败`, [String(error)], 'error');
    return undefined;
  }
}

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  output = vscode.window.createOutputChannel('ld-java-analyze');
  context.subscriptions.push(output);
  const configuration = vscode.workspace.getConfiguration('ld-java-analyze');
  const command = serverPath(context);
  const serverAvailable = existsSync(command);
  if (!serverAvailable) {
    report(
      'LSP 可执行文件缺失',
      [`${command}`, '请在 ld-java-analyze.serverPath 中指定绝对路径'],
      'error',
    );
  }
  const serverOptions: ServerOptions = {
    run: { command, transport: TransportKind.stdio },
    debug: { command, transport: TransportKind.stdio },
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: 'file', language: 'java' },
      { scheme: 'file', language: 'xml' },
    ],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher('**/*.{java,xml}'),
      configurationSection: 'ld-java-analyze',
    },
    initializationOptions: {
      referenceRoots: configuration.get<string[]>('referenceRoots') ?? [],
      excludeRules: configuration.get<string[]>('excludeRules') ?? [],
      useBuiltinExcludes: configuration.get<boolean>('useBuiltinExcludes') ?? true,
      logDir: configuration.get<string>('logDir') ?? '',
    },
  };

  // Register commands immediately; do not block them on the LSP handshake.
  // `call()` awaits `clientReady` only when a command actually needs the server.
  if (serverAvailable) {
    client = new LanguageClient('ld-java-analyze', 'ld-java-analyze', serverOptions, clientOptions);
    clientReady = client.start();
    clientReady.catch((error: unknown) => {
      report('语言服务启动失败', [String(error)], 'error');
    });
  }

  context.subscriptions.push(
    vscode.commands.registerCommand('ld-java-analyze.referencesInPaths', async () => {
      const position = activePosition();
      if (!position) {
        return;
      }
      const roots = await pickRoots(position.uri);
      if (roots === undefined) {
        return;
      }
      const response = await call<{ locations: ReferenceHit[]; total: number }>(
        'ld-java-analyze.referencesInPaths',
        {
          uri: position.uri,
          line: position.line,
          character: position.character,
          roots,
          includeDeclaration: true,
        },
      );
      if (!response) {
        return; // call() already reported the failure
      }
      if (response.total === 0) {
        report(
          '引用查询完成',
          [
            roots.length === 0
              ? '工作区内没有找到引用'
              : `所选 ${roots.length} 个目录下没有引用`,
            `光标 ${path.basename(vscode.Uri.parse(position.uri).fsPath)}:${position.line + 1}:${position.character + 1}`,
          ],
          'warn',
        );
        return;
      }
      writeOutput('引用查询完成', [
        `找到 ${response.total} 处引用`,
        `过滤目录 ${roots.length === 0 ? '（整个工作区）' : roots.join(', ')}`,
      ]);

      const toRange = (range: LspRange) =>
        new vscode.Range(
          new vscode.Position(range.start.line, range.start.character),
          new vscode.Position(range.end.line, range.end.character),
        );
      const locations = response.locations.map(
        (hit) => new vscode.Location(vscode.Uri.parse(hit.uri), toRange(hit.range)),
      );
      const items: ResultItem[] = [
        {
          label: `$(list-tree) 在「引用」面板中查看全部 ${response.total} 处`,
          description: 'VS Code 原生 Peek 视图',
          peek: true,
        },
        ...response.locations.map((hit, index) => {
          const range = toRange(hit.range);
          const uri = vscode.Uri.parse(hit.uri);
          return {
            label: `${path.basename(uri.fsPath)}:${range.start.line + 1}:${range.start.character + 1}`,
            description: vscode.workspace.asRelativePath(uri),
            detail: hit.role,
            location: locations[index],
          } satisfies ResultItem;
        }),
      ];
      const chosen = await vscode.window.showQuickPick(items, {
        placeHolder: `找到 ${response.total} 处引用（选择一项跳转）`,
        matchOnDescription: true,
      });
      if (!chosen) {
        return;
      }
      if (chosen.peek) {
        try {
          await vscode.commands.executeCommand(
            'editor.action.showReferences',
            vscode.Uri.parse(position.uri),
            new vscode.Position(position.line, position.character),
            locations,
          );
        } catch (error) {
          report('引用面板不可用', [String(error), '可改用列表中的单条结果跳转'], 'warn');
        }
        return;
      }
      if (chosen.location) {
        const document = await vscode.workspace.openTextDocument(chosen.location.uri);
        const editor = await vscode.window.showTextDocument(document);
        editor.selection = new vscode.Selection(
          chosen.location.range.start,
          chosen.location.range.start,
        );
        editor.revealRange(
          chosen.location.range,
          vscode.TextEditorRevealType.InCenter,
        );
      }
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('ld-java-analyze.setReferenceRoots', async () => {
      const roots = await pickRoots();
      if (roots === undefined) {
        return;
      }
      await vscode.workspace
        .getConfiguration('ld-java-analyze')
        .update('referenceRoots', roots, vscode.ConfigurationTarget.Workspace);
      const response = await call<{ referenceRoots: string[] }>(
        'ld-java-analyze.setReferenceRoots',
        roots,
      );
      if (!response) {
        return;
      }
      report('引用过滤目录已更新', [
        roots.length === 0
          ? '已清除过滤目录，引用将列出整个工作区'
          : `已设置 ${roots.length} 个过滤目录`,
        ...roots,
      ]);
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('ld-java-analyze.jarCandidates', async () => {
      const position = activePosition();
      if (!position) {
        return;
      }
      const response = await call<{ fqn: string; candidates: JarHit[] }>(
        'ld-java-analyze.jarCandidates',
        {
          uri: position.uri,
          line: position.line,
          character: position.character,
        },
      );
      if (!response) {
        return;
      }
      if (response.candidates.length === 0) {
        report(
          'JAR 候选查询完成',
          [
            `未找到 ${response.fqn} 对应的本地 JAR`,
            '请确认 <workspace>/.ld-java-analyze/classpath.txt 与同目录的 *-sources.jar',
          ],
          'warn',
        );
        return;
      }
      writeOutput('JAR 候选查询完成', [
        `fqn=${response.fqn}`,
        `候选 ${response.candidates.length} 个：${response.candidates.map((c) => c.jar).join(', ')}`,
      ]);
      const items = response.candidates.map((candidate) => ({
        label: candidate.label,
        description: candidate.jar,
        uri: candidate.uri,
        line: candidate.line,
        hasSource: candidate.hasSource,
      }));
      const picked = await vscode.window.showQuickPick(items, {
        placeHolder: `选择要跳转的 JAR（${response.fqn}）`,
      });
      if (!picked) {
        return;
      }
      try {
        const document = await vscode.workspace.openTextDocument(
          vscode.Uri.parse(picked.uri),
        );
        const editor = await vscode.window.showTextDocument(document);
        const positionInFile = new vscode.Position(picked.line, 0);
        editor.selection = new vscode.Selection(positionInFile, positionInFile);
        editor.revealRange(
          new vscode.Range(positionInFile, positionInFile),
          vscode.TextEditorRevealType.InCenter,
        );
      } catch (error) {
        report('打开 JAR 目标失败', [String(error), picked.uri], 'error');
      }
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('ld-java-analyze.editExcludeRules', async () => {
      const file = await ensureIgnoreFile();
      if (!file) {
        return;
      }
      try {
        const document = await vscode.workspace.openTextDocument(vscode.Uri.file(file));
        await vscode.window.showTextDocument(document);
        report('扫描排除规则', [`已打开 ${file}`, '保存后自动重建索引']);
      } catch (error) {
        report('打开规则文件失败', [String(error)], 'error');
      }
    }),
  );

  context.subscriptions.push(
    vscode.workspace.onDidSaveTextDocument(async (document) => {
      const file = ignoreFilePath();
      if (!file || !document.uri.fsPath) {
        return;
      }
      if (path.resolve(document.uri.fsPath).toLowerCase() !== path.resolve(file).toLowerCase()) {
        return;
      }
      await call('ld-java-analyze.reindex', {});
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('ld-java-analyze.setLogDir', async () => {
      const picked = await vscode.window.showOpenDialog({
        canSelectFiles: false,
        canSelectFolders: true,
        canSelectMany: false,
        openLabel: '选择日志输出目录',
      });
      if (!picked?.[0]) {
        report('已取消日志目录选择', ['保持原有日志配置'], 'warn');
        return;
      }
      const value = picked[0].fsPath;
      await vscode.workspace
        .getConfiguration('ld-java-analyze')
        .update('logDir', value, vscode.ConfigurationTarget.Workspace);
      const applied = await call<{ enabled: boolean; retentionDays: number }>(
        'ld-java-analyze.setLogDir',
        value,
      );
      if (!applied) {
        return;
      }
      report('日志目录已更新', [
        value,
        `启用=${applied.enabled}，保留 ${applied.retentionDays} 天`,
      ]);
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('ld-java-analyze.reindex', async () => {
      const result = await call<{
        files: number;
        changed: number;
        symbols: number;
        occurrences: number;
        links: number;
      }>('ld-java-analyze.reindex', {});
      if (!result) {
        return;
      }
      report('索引已重建', [
        `${result.files} 个文件 / ${result.symbols} 个符号 / ${result.links} 条框架链接`,
        `本次重解析 ${result.changed} 个文件，出现 ${result.occurrences} 条`,
      ]);
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('ld-java-analyze.stats', async () => {
      const stats = await call<Record<string, unknown>>('ld-java-analyze.stats', {});
      if (!stats) {
        return;
      }
      const entries = Object.entries(stats).map(([key, value]) => ({
        label: key,
        description:
          value === null || value === undefined
            ? '(未设置)'
            : Array.isArray(value) || typeof value === 'object'
              ? JSON.stringify(value)
              : String(value),
      }));
      const lines = entries.map((entry) => `${entry.label} = ${entry.description}`);
      writeOutput('索引状态', [
        `files=${stats.files} symbols=${stats.symbols} occurrences=${stats.occurrences}`,
        ...lines,
      ]);
      // A QuickPick is impossible to miss, unlike a transient notification.
      await vscode.window.showQuickPick(entries, {
        placeHolder: `ld-java-analyze 索引状态：${stats.files} 文件 / ${stats.symbols} 符号（Esc 关闭）`,
        matchOnDescription: true,
      });
      report(
        '索引状态已输出',
        [`${stats.files} 文件 / ${stats.symbols} 符号，完整字段见「ld-java-analyze」输出面板`],
        'info',
      );
    }),
  );

  context.subscriptions.push(
    vscode.commands.registerCommand('ld-java-analyze.selfTest', async () => {
      const configuration = vscode.workspace.getConfiguration('ld-java-analyze');
      const editor = vscode.window.activeTextEditor;
      const sourceDocument = editor
        && (editor.document.languageId === 'java' || editor.document.languageId === 'xml')
        ? editor
        : undefined;
      const position = sourceDocument
        ? {
            uri: sourceDocument.document.uri.toString(),
            line: sourceDocument.selection.active.line,
            character: sourceDocument.selection.active.character,
          }
        : undefined;
      const results: string[] = [];
      const probe = async (label: string, name: string, argument: unknown) => {
        const value = await call<unknown>(name, argument);
        results.push(`${value === undefined ? 'FAIL' : 'OK  '} ${label} (${name})`);
      };
      await probe('索引状态', 'ld-java-analyze.stats', {});
      await probe('排除规则', 'ld-java-analyze.getExcludeRules', {});
      await probe('重建索引', 'ld-java-analyze.reindex', {});
      if (position) {
        await probe('按目录查引用', 'ld-java-analyze.referencesInPaths', {
          uri: position.uri,
          line: position.line,
          character: position.character,
          roots: [],
          includeDeclaration: true,
        });
        await probe('JAR 候选', 'ld-java-analyze.jarCandidates', {
          uri: position.uri,
          line: position.line,
          character: position.character,
        });
      } else {
        results.push(
          editor
            ? `SKIP 光标相关命令（当前文档为 ${editor.document.languageId}，需先打开 Java/XML 文件）`
            : 'SKIP 光标相关命令（未打开编辑器）',
        );
      }
      // Idempotent write-backs: keep the current configuration unchanged.
      await probe('设置引用目录', 'ld-java-analyze.setReferenceRoots',
        configuration.get<string[]>('referenceRoots') ?? []);
      await probe('设置排除规则', 'ld-java-analyze.setExcludeRules',
        configuration.get<string[]>('excludeRules') ?? []);

      const failures = results.filter((line) => line.startsWith('FAIL')).length;
      writeOutput('命令自检', results);
      await vscode.window.showQuickPick(
        results.map((line) => ({ label: line })),
        {
          placeHolder: `ld-java-analyze 命令自检：${failures === 0 ? '全部通过' : `${failures} 项失败`}（Esc 关闭）`,
        },
      );
      report(
        '命令自检完成',
        [failures === 0 ? '全部通过' : `${failures} 项失败，详见「ld-java-analyze」输出面板`, ...results],
        failures === 0 ? 'info' : 'error',
      );
    }),
  );
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
