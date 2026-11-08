# AGENT.md — ld-java-analyze 迭代规范

本文件是**维护者/AI 助手**的唯一规范入口，写的是代码与配置里看不出来的约定。
使用者流程看 `README.md`；命令以 `editors/vscode/package.json` 的 `scripts` 和
`.github/workflows/ci.yml` 为准。改行为时只改一个事实来源，避免在文档里复制命令。

## 事实来源指针

| 遇到这些分支时 | 读 |
| --- | --- |
| 改构建、打包、安装流程；新增发布步骤 | `README.md` 的「快速上手」与「从源码构建」 |
| 改 classpath / sources jar 生成流程 | `scripts/setup-classpath.ps1` 与 `scripts/setup-classpath.sh`（README 只讲用法） |
| 新增/改名自定义命令、改 capabilities | `crates/ld-java-analyze/src/backend.rs` 的 `COMMANDS` 与 `initialize` |
| 改扫描规则、配置项、保留策略 | `crates/nav-core/src/scan_rules.rs` 与 `crates/ld-java-analyze/src/logger.rs` |
| 改名字绑定、重载选择、索引查询 | `crates/nav-index/src/lib.rs` 的 `resolve_unresolved` / `symbol_at` |
| 加框架跳转 | `crates/nav-java/src/mybatis.rs`（`Link` 范式）、`lombok.rs`（合成符号范式） |
| 改验证闸门 | `.github/workflows/ci.yml` |
| 解答本机安装/试用问题 | `README.md` 的「快速上手」与「验证与排障」 |

## 硬边界

这些是产品契约，改动前先确认用户是否调整了需求：

- 插件**只读**本地产物：不启动 `mvn`、不联网、不下载源码。`classpath.txt` 与
  `*-sources.jar` 由用户用 Maven 生成。
- 运行期状态默认只写 `<VS Code 工作区根>/.ld-java-analyze/`。`logDir` 是唯一可由配置指定
  到工作区之外的路径；其余索引、缓存、规则都留在工作区。
- LSP 的 stdout **只承载 JSON-RPC**。诊断信息写文件日志；不可用 `println!` / stderr 打印业务日志。
- 语言范围以 Java 8 为主，额外最小支持 **record**（类符号 + 组件访问器）。
  扩展其它 Java 16+ 语法前先评估 `tree-sitter-java 0.23.5` 的节点结构。
- 框架层保持薄：MyBatis 只做 XML ↔ 接口方法双向 `Link`，Lombok 只注入最小合成符号；
  Spring Boot / MapStruct 复用普通 `implementation` 与方法跳转，不建 provider。
- `executeCommandProvider.commands` **保持空数组**。原因见「已知陷阱」。
- 「工作区优先」：目标类型在工作区有源码时，定义跳转不降级到 JAR 源码或字节码结构视图；
  显式查看 JAR 走 `ld-java-analyze.jarCandidates`。

## 分层

依赖单向，禁止反向引用：

```
nav-core  (数据模型 / ScanRules / URI 工具)
   ↑
nav-java (解析 · Lombok · MyBatis)   nav-maven (只读发现)   nav-jar (JAR 候选)
   ↑                                        ↑                    ↑
        nav-index (唯一索引真相源 · 引用绑定 · 快照)
                          ↑
              ld-java-analyze (LSP handler · 日志 · 转换)
                          ↑
              editors/vscode (薄客户端 · UI · 打包)
```

- `WorkspaceIndex` 是所有导航能力的唯一真相源；框架 provider 只返回 `Link`，不直接改索引。
- `SymbolId` 由 `symbol_id()` 稳定派生；`FileContribution` 是可重建的最小单元。
- 解析器输出结构变化（新增字段、改变 occurrence/unresolved 语义）时，
  **`INDEX_VERSION` 必须 +1**，否则旧快照会让修复只对重新解析的文件生效。

## 迭代步骤与完成标准

1. **定位**：按上面的指针读取相关文件，确认要改的是 Rust 层、LSP 层还是扩展层。
   完成标准：能说清改动落在哪一层、是否触碰 `FileContribution` / 对外配置 / 命令 ID。
2. **实现**：Rust 改动保持分层单向；扩展改动保持「命令注册不依赖 LSP 启动完成」。
   完成标准：无跨层反向依赖，新增配置有默认值且兼容旧快照。
3. **闸门**：按 CI 的同一组检查本地跑一遍（`cargo fmt --all -- --check`、
   `cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、
   `npm run typecheck`）。完成标准：四项全绿、无 warning。
   本地同步扩展依赖用 `npm install`；`npm ci` 会先批量删除 `node_modules`，
   在本机安全策略下会被拦截，只适用于 CI 的干净 checkout。
4. **端到端**：`node testdata/verify-lsp.mjs`。完成标准：全部 PASS、退出码 0。
   新增 LSP 能力时在脚本里补对应断言，让回归可复现。
5. **打包**：先构建 `ld-java-analyze` 并复制到 `editors/vscode/server/`，再 `npm run vsix`、
   `npm run install:local`。完成标准：vsix 内含 `server/ld-java-analyze(.exe)` 与 `LICENSE`。
6. **真实项目验证**：在用户的 `demo1` 工作区 Reload Window 后跑
   `ld-java-analyze: 自检所有命令`。完成标准：无 FAIL，`ld-java-analyze.stats` 的 `files` / `compiledJars`
   与项目实际一致，`.ld-java-analyze/logs/` 有本次启动记录且无误导性 WARN。

版本号在 `Cargo.toml [workspace.package]`、`editors/vscode/package.json`、
vsix 文件名三处同步。`LICENSE` 在仓库根与 `editors/vscode/` 各一份，内容保持一致。

## 用户可见提示规范

- **命名全量统一为 `ld-java-analyze`**：扩展标识、显示名、命令标题前缀、
  配置节 `ld-java-analyze.*`、命令 ID、LSP 命令名、`configurationSection`、
  输出面板名、日志文件名前缀。旧的 `javaNav.*` 别名已全部移除，不保留兼容；
  新增配置或命令一律用新前缀。
- 工作区状态目录统一为 `<工作区根>/.ld-java-analyze/`（classpath / config / ignore /
  cache / logs）。它始终排除且不可 `!` 反选，否则从 sources jar 解出的缓存源码会回流污染索引。
  新增运行期产物一律放这里，不写用户目录；改这个目录名要同步迁移已有工作区的数据。
- VS Code 通知统一以 `ld-java-analyze: ` 开头，中文、一句话一个动作；错误必须给出**可执行的下一步**
  （`Reload Window`、检查 `.ld-java-analyze/classpath.txt`、查看输出面板或日志）。
- 每个命令的结果都写入 **「ld-java-analyze」输出面板**（`writeOutput` / `report`），
  通知只是补充。取消、空结果、服务未就绪、请求失败都要有可见反馈；回调里不留裸 `return`。
- 新命令三件事一起做：`contributes.commands` 注册、`activationEvents` 加 `onCommand:<id>`、
  `extension.ts` 里 `registerCommand`。命令注册放在 `client.start()` 之前，
  需要服务端时用 `call()`（它等待 `clientReady` 并统一处理 `null` 与异常）。
- UI 交互优先用 `showQuickPick`（目录选择、结果列表、状态字段），系统文件对话框作为备选。
- 日志格式 `HH:MM:SS.mmm [级别] [模块] 内容`，UTC、按天分文件、保留 7 天、启动清理。
  级别约定：`INFO` = 正常决策与结果（含「无目标」），`WARN` = 真正异常或降级，
  `ERROR` = 解析失败、启动失败、打开目标失败。
- 每个命令在服务端都要走到统一的「完成」日志；命令分支内用值绑定，避免提前 `return`
  跳过完成记录。

## 回归测试归属

| 改动 | 补测试的位置 |
| --- | --- |
| 语法特性、注解、record、调用分类 | `crates/nav-java/src/parser.rs` 的 `tests` |
| 名字绑定、重载、目录过滤、增量、快照 | `crates/nav-index/src/lib.rs` 的 `tests` |
| 排除规则、反选、配置解析 | `crates/nav-core/src/scan_rules.rs` / `crates/ld-java-analyze/src/convert.rs` |
| 日志清理、日期、配置回退 | `crates/ld-java-analyze/src/logger.rs` 的 `tests` |
| JAR 候选、源码优先、结构视图 | `crates/nav-jar/src/lib.rs` 的 `tests` |
| 端到端 LSP 能力 | `testdata/verify-lsp.mjs` 加断言 |

`testdata/demo1` 是刻意保留的验收工程：含 record/lombok 之外的各种边界
（`generated/` 目录、`Temp.generated.java`、`other/ExternalCaller.java`），
改扫描规则或引用过滤时用它验证，别用真实业务项目替代。

## 已知陷阱

代码里看不出来、但每次都容易踩的约定：

- **`vscode-languageclient` 抢注命令**：`ExecuteCommandFeature` 会为
  `executeCommandProvider.commands` 每一项自动注册同名 VS Code 命令并把参数原样转发，
  遮蔽扩展自己的 UI。服务端声明保持空数组；曾表现为右键点击只把 `Uri` 对象
  (`{"$mid":1,...}`) 发到服务端、不弹任何界面。
- **索引快照**：`INDEX_VERSION` 不变时，`scan()` 只按文件 stamp 重解析变化的文件。
  改了解析语义而忘记升版本，会看到「代码改了但跳转没变」。
- **PowerShell `-D` 参数**：`mvn -Dmdep.outputFile=...` 会被拆成两个参数。
  `scripts/setup-classpath.ps1` 把参数作为带引号的数组元素传递，手写命令必须加引号或用 `--%`。
- **多模块 reactor**：`dependency:build-classpath` 对每个模块各执行一次，相对 `outputFile`
  会散落到各模块目录，根那份只来自父 pom。脚本已用 `-pl` + 绝对路径 + 逐模块探测封装，
  改这一逻辑时改脚本，别在 README 里复制命令。
- **`.ld-java-analyze` 始终排除**且不可 `!` 反选：从 sources jar 解出的缓存源码若被索引会回流污染。
- **库跳转**：有 sources jar 时只返回源码候选；`has_workspace_type` 守卫阻止工作区已有类型降级到 JAR。
- **扩展更新必须 Reload Window**：`code --install-extension` 不会让已运行的窗口加载新 `extension.js`。
- **打包前复制 server**：`server/` 被 gitignore，vsix 只包含当时目录里的二进制；
  只跑 `npm run vsix` 而不复制新 `ld-java-analyze` 会装到旧 server。
- **配置文件与客户端设置的优先级**：`initializationOptions.logDir` 非空时优先；
  空字符串视为「未指定」并回退 `<workspace>/.ld-java-analyze/config.json`（VS Code 默认总传空值）。
  `ld-java-analyze.setLogDir` 是显式的运行时切换。
- **vsce 的两条警告**：本地扩展没有远程仓库，脚本用 `--allow-missing-repository` 消除，
  不要为此编造 URL；`should bundle your extension` 是性能建议，当前刻意保留未打包的
  `vscode-languageclient` 依赖链便于调试，未来引入 esbuild 时再改构建脚本与 `.vscodeignore`。
