const http = require('http');
const path = require('path');
const vscode = require('vscode');

// Each Cursor window gets its own extension host, so each window needs
// a dedicated localhost port. oc-claw uses this to bind a session to a
// specific Cursor window instead of broadcasting to every window forever.
const PORT_BASE = 23456;
const PORT_RANGE = 5;

let server = null;
let boundPort = null;

function getWorkspaceRoots() {
    return (vscode.workspace.workspaceFolders || []).map(folder => folder.uri.fsPath);
}

function getWorkspaceName() {
    if (typeof vscode.workspace.name === 'string' && vscode.workspace.name) {
        return vscode.workspace.name;
    }

    const roots = getWorkspaceRoots();
    if (roots.length > 0) {
        return path.basename(roots[0]);
    }

    return '';
}

function getNativeHandleHex() {
    try {
        if (!vscode.window.nativeHandle) return null;
        return Buffer.from(vscode.window.nativeHandle).toString('hex');
    } catch {
        return null;
    }
}

function getWindowMeta() {
    return {
        port: boundPort,
        focused: vscode.window.state.focused,
        workspaceName: getWorkspaceName(),
        workspaceRoots: getWorkspaceRoots(),
        nativeHandle: getNativeHandleHex(),
    };
}

async function focusWindow() {
    // Prefer the active terminal in this Cursor window because the jump
    // action conceptually returns the user to the agent's terminal context.
    const terminal = vscode.window.activeTerminal || vscode.window.terminals[0];
    if (terminal) {
        terminal.show(false);
        return true;
    }

    // If there is no terminal, fall back to the active editor so the
    // correct Cursor window is still surfaced to the user.
    const editor = vscode.window.activeTextEditor;
    if (editor) {
        await vscode.window.showTextDocument(editor.document, {
            preserveFocus: false,
            preview: false,
            viewColumn: editor.viewColumn,
        });
        return true;
    }

    return false;
}

async function focusTerminalByPids(pids) {
    for (const terminal of vscode.window.terminals) {
        const termPid = await terminal.processId;
        if (termPid && pids.includes(termPid)) {
            terminal.show(false);
            return true;
        }
    }
    return false;
}

function tryListen(port, maxPort) {
    if (port > maxPort) {
        console.log('oc-claw terminal-focus: all ports in use, HTTP server disabled');
        return;
    }

    server = http.createServer((req, res) => {
        if (req.method === 'GET' && req.url === '/window-meta') {
            res.writeHead(200, { 'Content-Type': 'application/json' });
            res.end(JSON.stringify(getWindowMeta()));
            return;
        }

        if (req.method !== 'POST') {
            res.writeHead(404);
            res.end();
            return;
        }

        let body = '';
        req.on('data', (chunk) => { body += chunk; });
        req.on('end', async () => {
            try {
                if (req.url === '/focus-window') {
                    const focused = await focusWindow();
                    res.writeHead(focused ? 200 : 404);
                    res.end(focused ? 'ok' : 'not found');
                    return;
                }

                if (req.url === '/focus-tab') {
                    const data = body ? JSON.parse(body) : {};
                    const pids = Array.isArray(data.pids) ? data.pids.filter(Number.isFinite) : [];
                    if (!pids.length) {
                        res.writeHead(400);
                        res.end('no pids');
                        return;
                    }

                    const found = await focusTerminalByPids(pids);
                    res.writeHead(found ? 200 : 404);
                    res.end(found ? 'ok' : 'not found');
                    return;
                }

                res.writeHead(404);
                res.end();
            } catch {
                res.writeHead(400);
                res.end('bad json');
            }
        });
    });

    server.on('error', (err) => {
        if (err.code === 'EADDRINUSE') {
            server = null;
            tryListen(port + 1, maxPort);
        }
    });

    server.listen(port, '127.0.0.1', () => {
        boundPort = port;
        console.log(`oc-claw terminal-focus: listening on 127.0.0.1:${port}`);
    });
}

function activate() {
    tryListen(PORT_BASE, PORT_BASE + PORT_RANGE - 1);
}

function deactivate() {
    if (server) {
        server.close();
        server = null;
    }
    boundPort = null;
}

module.exports = { activate, deactivate };
