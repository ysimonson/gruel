import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export function activate(context: vscode.ExtensionContext) {
  const config = vscode.workspace.getConfiguration("gruel");
  const serverPath = config.get<string>("serverPath", "gruel");
  const previewFeatures = config.get<string[]>("previewFeatures", [
    "language_server",
  ]);

  const args = ["lsp"];
  for (const feature of previewFeatures) {
    args.push("--preview", feature);
  }

  const serverOptions: ServerOptions = {
    command: serverPath,
    args,
    options: { shell: false },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "gruel" }],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher("**/*.gruel"),
    },
  };

  client = new LanguageClient(
    "gruel-lsp",
    "Gruel Language Server",
    serverOptions,
    clientOptions
  );

  client.start();
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
