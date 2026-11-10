# ld-java-next

**Java 8+ 本地代码导航插件**：VS Code 扩展 + Rust LSP Server，通过 stdio JSON-RPC 通信。

## 当前结构

```text
crates/nav-next       下一代索引内核
crates/ld-java-next   独立 LSP Server
editors/vscode-next   VS Code 扩展
docs/                 设计与实施记录
```

## 功能

- 定义跳转
- 实现跳转
- 引用查找
- 文件符号
- 工作区符号
- 方法全路径搜索，支持 `.` 和 `#`
- 指定目录强制刷新
- VS Code 明文桥，用于读取本机加密文件
- `redb 4.5.0` 持久化
- `tree-sitter-java` 解析
- `ArcSwap` 不可变快照

## 构建

```powershell
cargo build --workspace --release
```

## 打包扩展

```powershell
cd editors/vscode-next
npm install
npm run vsix
```

## 安装

```powershell
code --install-extension editors/vscode-next/ld-java-next-0.1.0.vsix --force
```

## 运行期状态

默认写入：

```text
<工作区根>/.ld-java-next/
```

不会写入用户目录，不会启动 Maven，不会联网。
