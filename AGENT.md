# AGENT.md — ld-java-next 维护规范

本仓库现在是**从零重写版**，只保留新实现：

```text
crates/nav-next       下一代索引内核
crates/ld-java-next   独立 LSP Server
editors/vscode-next   VS Code 扩展
docs/                 设计与实施记录
```

不要再引用或恢复旧目录：

```text
crates/nav-core
crates/nav-java
crates/nav-index
crates/nav-maven
crates/nav-jar
crates/ld-java-analyze
editors/vscode
scripts
testdata
```

## 硬边界

- 不复用旧索引实现。
- 不启动 Maven，不联网，不写用户目录。
- 运行期状态只写 `<工作区根>/.ld-java-next/`。
- LSP stdout 只承载 JSON-RPC。
- 扩展与服务端的命令注册不要重名；服务端 `executeCommandProvider.commands` 保持空数组。

## 构建与验证

- **不要**为每次构建创建新的 `target/xxx` 目录。
- 统一使用默认 `target/`，需要清理时只用 `cargo clean`。
- 不新增自定义“冒烟脚本”；功能验证由用户在 VS Code 里手动点击完成。

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

扩展：

```powershell
cd editors/vscode-next
npm run typecheck
```
