import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import {
    Executable,
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    TransportKind,
} from 'vscode-languageclient/node';

interface ClientDocument {
    uri: string;
    text?: string;
    error?: string;
}

interface ReadTextDocumentsParams {
    uris?: string[];
}

interface ScanProgressParams {
    phase: string;
    processed: number;
    total: number;
}

interface MethodHit {
    name: string;
    fqn: string;
    descriptor: string;
    uri?: string;
    range?: {
        start: { line: number; character: number };
        end: { line: number; character: number };
    };
}

let client: LanguageClient | undefined;

export async function activate(context: vscode.ExtensionContext): Promise<void> {
    const serverPath = resolveServerPath(context);
    if (!serverPath) {
        vscode.window.showErrorMessage(
            'ld-java-next: server executable not found. Set ld-java-next.serverPath or bundle server/ld-java-next.'
        );
        return;
    }

    const executable: Executable = {
        command: serverPath,
        transport: TransportKind.stdio,
    };
    const serverOptions: ServerOptions = {
        run: executable,
        debug: executable,
    };

    const outputChannel = vscode.window.createOutputChannel('ld-java-next');
    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ language: 'java' }],
        outputChannel,
        outputChannelName: 'ld-java-next',
        initializationOptions: {},
    };

    client = new LanguageClient(
        'ld-java-next',
        'ld-java-next',
        serverOptions,
        clientOptions
    );

    registerReadTextDocuments(client);
    registerScanProgress(context, client, outputChannel);
    registerCommands(context, client, outputChannel);

    await client.start();
}

export async function deactivate(): Promise<void> {
    if (client) {
        await client.stop();
    }
}

function resolveServerPath(context: vscode.ExtensionContext): string | undefined {
    const configured = vscode.workspace
        .getConfiguration('ld-java-next')
        .get<string>('serverPath', '');
    if (configured && configured.trim().length > 0) {
        return configured;
    }

    const executableName =
        process.platform === 'win32' ? 'ld-java-next.exe' : 'ld-java-next';
    const bundled = path.join(context.extensionPath, 'server', executableName);
    if (fs.existsSync(bundled)) {
        return bundled;
    }

    const development = path.join(
        context.extensionPath,
        '..',
        '..',
        'target',
        'debug',
        executableName
    );
    if (fs.existsSync(development)) {
        return development;
    }

    return undefined;
}

function registerReadTextDocuments(languageClient: LanguageClient): void {
    languageClient.onRequest(
        'ld-java-next/readTextDocuments',
        async (params: ReadTextDocumentsParams) => {
            const uris = params.uris ?? [];
            const documents: ClientDocument[] = await Promise.all(
                uris.map(async (uri) => {
                    try {
                        const document = await vscode.workspace.openTextDocument(
                            vscode.Uri.parse(uri)
                        );
                        return { uri, text: document.getText() };
                    } catch (openError) {
                        try {
                            const bytes = await vscode.workspace.fs.readFile(
                                vscode.Uri.parse(uri)
                            );
                            return { uri, text: Buffer.from(bytes).toString('utf8') };
                        } catch (fsError) {
                            return {
                                uri,
                                error: String(fsError ?? openError),
                            };
                        }
                    }
                })
            );
            return documents;
        }
    );
}

function registerScanProgress(
    context: vscode.ExtensionContext,
    languageClient: LanguageClient,
    outputChannel: vscode.OutputChannel
): void {
    const status = vscode.window.createStatusBarItem(
        vscode.StatusBarAlignment.Left,
        100
    );
    status.name = 'ld-java-next scan progress';
    context.subscriptions.push(status);

    languageClient.onNotification(
        'ld-java-next/scanProgress',
        (params: ScanProgressParams) => {
            const phaseText =
                params.phase === 'discover'
                    ? '发现文件'
                    : params.phase === 'read'
                      ? '读取文件'
                      : params.phase === 'clientRead'
                        ? 'VS Code 读取'
                        : params.phase === 'index'
                          ? '建立索引'
                          : params.phase === 'complete'
                            ? '扫描完成'
                            : params.phase;
            const text = `${phaseText} ${params.processed}/${params.total}`;
            status.text = `$(search) ld-java-next: ${text}`;
            status.tooltip = text;
            status.show();
            outputChannel.appendLine(`[scan] ${text}`);

            if (params.phase === 'complete') {
                setTimeout(() => status.hide(), 5000);
            }
        }
    );
}

function registerCommands(
    context: vscode.ExtensionContext,
    languageClient: LanguageClient,
    outputChannel: vscode.OutputChannel
): void {
    context.subscriptions.push(
        vscode.commands.registerCommand('ld-java-next.searchMethods', async () => {
            const query = await vscode.window.showInputBox({
                prompt: 'Method full path, for example com.example.UserService#findById',
                placeHolder: 'com.example.UserService#findById',
            });
            if (!query) {
                return;
            }

            const response = await languageClient.sendRequest<any>(
                'workspace/executeCommand',
                {
                    command: 'ld-java-next.searchMethods',
                    arguments: [query],
                }
            );
            const methods: MethodHit[] = response?.methods ?? [];
            if (methods.length === 0) {
                vscode.window.showInformationMessage('ld-java-next: no methods matched');
                return;
            }

            const picked = await vscode.window.showQuickPick(
                methods.map((method) => ({
                    label: method.fqn,
                    description: method.descriptor,
                    method,
                })),
                { placeHolder: 'Select a method' }
            );
            if (picked && picked.method.uri && picked.method.range) {
                const document = await vscode.workspace.openTextDocument(
                    vscode.Uri.parse(picked.method.uri)
                );
                await vscode.window.showTextDocument(document, {
                    selection: new vscode.Range(
                        new vscode.Position(
                            picked.method.range.start.line,
                            picked.method.range.start.character
                        ),
                        new vscode.Position(
                            picked.method.range.end.line,
                            picked.method.range.end.character
                        )
                    ),
                });
            }
        }),

        vscode.commands.registerCommand('ld-java-next.stats', async () => {
            const response = await languageClient.sendRequest<any>(
                'workspace/executeCommand',
                {
                    command: 'ld-java-next.stats',
                    arguments: [],
                }
            );
            const text = JSON.stringify(response, null, 2);
            outputChannel.appendLine(text);
            outputChannel.show();
            vscode.window.showInformationMessage('ld-java-next: stats written to output');
        }),

        vscode.commands.registerCommand('ld-java-next.reindex', async () => {
            await vscode.window.withProgress(
                { location: vscode.ProgressLocation.Notification },
                async () => {
                    await languageClient.sendRequest<any>(
                        'workspace/executeCommand',
                        {
                            command: 'ld-java-next.reindex',
                            arguments: [],
                        }
                    );
                }
            );
            vscode.window.showInformationMessage('ld-java-next: index rebuilt');
        }),

        vscode.commands.registerCommand('ld-java-next.reindexPaths', async () => {
            const folder = await vscode.window.showWorkspaceFolderPick({
                placeHolder: 'Choose a workspace folder to refresh',
            });
            if (!folder) {
                return;
            }
            const relative = await vscode.window.showInputBox({
                prompt: 'Directory to refresh, relative to the chosen workspace folder',
                placeHolder: 'src/main/java',
            });
            if (!relative) {
                return;
            }
            const target = path.isAbsolute(relative)
                ? relative
                : path.join(folder.uri.fsPath, relative);

            await vscode.window.withProgress(
                { location: vscode.ProgressLocation.Notification },
                async () => {
                    await languageClient.sendRequest<any>(
                        'workspace/executeCommand',
                        {
                            command: 'ld-java-next.reindexPaths',
                            arguments: [target],
                        }
                    );
                }
            );
            vscode.window.showInformationMessage(
                `ld-java-next: directory index refreshed (${target})`
            );
        })
    );
}
