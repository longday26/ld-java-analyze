# ld-java-analyze

[![CI](https://github.com/longday26/ld-java-analyze/actions/workflows/ci.yml/badge.svg)](https://github.com/longday26/ld-java-analyze/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-dea584.svg)](Cargo.toml)
[![VS Code](https://img.shields.io/badge/VS%20Code-%5E1.85-007ACC.svg)](editors/vscode/package.json)

**为 Java 8 / Maven 多模块项目打造的本地代码导航插件**：
VS Code 薄扩展 + 原生 Rust LSP server，通过 stdio JSON-RPC 通信。

它自建 `Symbol` / `Occurrence` / `Link` 索引，把大型项目里最容易失准的几件事做扎实：
引用按目录收敛、库方法优先跳源码、同名类跨多个 JAR 时列候选、
依赖源码缓存不会污染工作区索引。

> **设计边界**：插件不联网、不启动 `mvn`、不下载源码、不写 `~/.m2` 或用户目录。
> 只读你用 Maven 生成好的 `classpath.txt` 与 `*-sources.jar`，全部运行期状态写在
> `<工作区根>/.ld-java-analyze/`。

---

## 特性

### 代码导航

- 定义、引用、实现、类型定义、读写高亮、悬停、文件结构、工作区符号
- 跨文件调用按**接收者静态类型 + 实参个数**收窄，避免误绑到同名方法
- Lombok `@Data` / `@Getter` / `@Setter` 合成 getter/setter 与构造器；Java record 组件访问器
- MyBatis Mapper XML ↔ 接口方法**双向**跳转
- Spring bean 注入复用 `implementation`，MapStruct 复用普通方法跳转，无需额外 provider

### 依赖与库

- 本地 `*-sources.jar` **源码优先**；无源码时降级为字节码结构视图（非反编译）
- 多个 JAR 含同名类时列出全部候选
- 注解属性、枚举常量、类型声明都能定位到源码里的具体行

### 工程友好

- 引用可按目录过滤：静态配置 + QuickPick 临时选目录，未设置则列整个工作区
- `.gitignore` 风格的**目录名 / 文件名**排除规则，避免 `generated/`、工具缓存混入索引
- 所有状态留在工作区，可直接随项目清理、不会散落到用户目录
- 文件日志按 UTC 天分片、保留 7 天、启动自动清理，绝不写 stdout

---

## 快速上手

仓库已附带 `editors/vscode/ld-java-analyze-0.1.0.vsix`，clone 后即可安装；
想自己构建见「[从源码构建](#从源码构建)」。下面的 `$repo` 指本仓库，
`$project` 指**被索引的 Maven 项目根**（也就是 VS Code 要打开的工作区）。

```powershell
$repo    = "path\to\ld-java-analyze"
$project = "path\to\your-maven-project"
```

**1. 安装扩展并重载窗口**

```powershell
code --install-extension "$repo\editors\vscode\ld-java-analyze-0.1.0.vsix" --force
```

在 VS Code 执行 `Developer: Reload Window`，并以 `$project` 作为工作区打开。

**2. 生成依赖清单**（多模块项目也不用自己挑模块）

```powershell
cd $project
& "$repo\scripts\setup-classpath.ps1"
```

Linux / macOS / CI 用 `bash "$repo/scripts/setup-classpath.sh"`。
先加 `-DryRun` / `--dry-run` 可以只看命令、不跑 Maven、不写文件。

**3. 验证**

打开任意 `.java` / `.xml` 文件，命令面板执行：

- `ld-java-analyze: 自检所有命令` —— 应全部 OK
- `ld-java-analyze: 查看索引状态` —— 确认 `root`、`files`、`compiledJars` / `sourceJars`
- `ld-java-analyze: 重建索引` —— 生成 `classpath.txt` 后执行一次

命令结果同时写入 **「ld-java-analyze」输出面板**。没有 classpath 时，
项目内源码的定义 / 引用 / 高亮 / MyBatis / Lombok 仍可用；
库方法跳源码才需要第 2 步。

---

## 从源码构建

完整流程：**构建 LSP 产物 → 构建 VS Code 插件产物 → 打包安装 → 重载使用**。

前置条件：Rust ≥ 1.85、Node.js ≥ 18、VS Code CLI `code`、Maven（用于生成 classpath）。

**一条最小回路**（PowerShell，顺序不可颠倒）：

```powershell
cd $repo

# ① 构建并验证 LSP server
cargo build --workspace
cargo test  --workspace
node testdata/verify-lsp.mjs        # 端到端验证，全部 PASS 才可继续

# ② 构建 VS Code 插件，并把 server 放进扩展目录
Copy-Item target\debug\ld-java-analyze.exe editors\vscode\server\ld-java-analyze.exe -Force
cd editors\vscode
npm install                         # 幂等；CI 干净环境用 npm ci
npm run typecheck

# ③ 打包 + 安装到本机 VS Code
npm run vsix                        # 先 compile，再生成 ld-java-analyze-0.1.0.vsix
npm run install:local               # code --install-extension ... --force

# ④ 确认安装；然后到 VS Code 里 Reload Window
code --list-extensions --show-versions | Select-String ld-java-analyze
```

`npm run vsix` 已包含编译，所以只改 `src/extension.ts` 或 `package.json` 时重跑 ②–④；
改动 Rust 代码才需要重做 ① 的构建与 `Copy-Item`。

### 步骤 1 — 构建并验证 LSP 产物

```powershell
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
node testdata/verify-lsp.mjs
```

`verify-lsp.mjs` 用真实 stdio JSON-RPC 会话覆盖定义、引用、目录过滤、MyBatis 双向跳转、
Lombok、实现、高亮、悬停、符号与扫描规则。产物为 `target/debug/ld-java-analyze(.exe)`；
设置 `LD_JAVA_ANALYZE_SERVER` 可让脚本改为验证已安装扩展里的 server。

### 步骤 2 — 构建 VS Code 插件

扩展源码在 `editors/vscode/`：`src/extension.ts`（命令与 UI）、`package.json`
（配置项、命令、激活事件）、`README.md`（扩展详情页）。

```powershell
cd editors\vscode
npm install            # 安装/同步依赖，幂等
npm run typecheck      # 只检查类型
npm run compile        # 生成 out/extension.js
```

扩展优先使用自带的 `server/ld-java-analyze(.exe)`，回退到开发目录
`../../target/debug/ld-java-analyze(.exe)`。**修改 Rust 后必须重新复制二进制**到
`editors/vscode/server/`，否则 vsix 里装的还是旧 server。

开发调试可不打包：VS Code 打开本仓库，用「Run Extension」启动扩展开发宿主。

### 步骤 3 — 打包并安装

```powershell
cd editors\vscode
npm run vsix            # = npm run compile && vsce package --out ld-java-analyze-0.1.0.vsix
npm run install:local   # = code --install-extension ld-java-analyze-0.1.0.vsix --force
```

VSIX 内含 `out/extension.js`、`server/ld-java-analyze(.exe)`、`LICENSE.txt`、`readme.md`，
安装后不需要 Rust 工具链。扩展 ID 为 `local.ld-java-analyze`。

### 步骤 4 — 重载并使用

1. `Developer: Reload Window` —— 已运行的窗口必须重载才会加载新 `extension.js`。
2. 打开**被索引项目根**作为工作区（不是本仓库）；插件只读 `<工作区根>/.ld-java-analyze/`。
3. 打开任意 `.java` / `.xml` 即激活，跑 `ld-java-analyze: 自检所有命令`。
4. 按「快速上手」第 2 步生成 `classpath.txt` 后执行 `ld-java-analyze: 重建索引`
   （文件监听只覆盖 `*.java` / `*.xml`，`.txt` 变化不会自动重读）。

---

## 配置

插件只读以下用户生成的文件，全部状态位于 `<工作区根>/.ld-java-analyze/`：

```
<workspace>/.ld-java-analyze/
├── classpath.txt    用户用 Maven 生成的只读 classpath
├── ignore           可选的扫描排除规则
├── config.json      可选的日志配置
├── cache/           索引快照、解出的 .java、无源码时的结构视图
└── logs/            启用 logDir 后的按天日志
```

### classpath 与源码 jar

`scripts/setup-classpath.ps1`（Windows）/ `setup-classpath.sh`（Linux、macOS、CI）
在**运行目录**自动完成，无需手写绝对路径或模块名：

1. 创建 `<当前目录>/.ld-java-analyze/`。
2. `mvn -q -U install -DskipTests` —— 安装 reactor 内部 SNAPSHOT；`-U` 清失败缓存。
3. `mvn -q dependency:sources` —— 下载 `*-sources.jar`。
4. 读取根 `pom.xml` 的 `<modules>`，逐个以 `-pl <module>` + **绝对路径临时文件**探测，
   取 jar 数最多的一份写入 `classpath.txt`；单模块项目跳过探测。临时文件不会散落到子模块。

| PowerShell | bash | 说明 |
| --- | --- | --- |
| `-Module demo1-web` | `--module demo1-web` | 指定最终模块，跳过探测（最快） |
| `-DryRun` | `--dry-run` | 只打印命令，不跑 Maven、不写文件 |
| `-SkipInstall` | `--skip-install` | 内部 SNAPSHOT 已安装时跳过 |
| `-SkipSources` | `--skip-sources` | 跳过 sources jar 下载 |

结束后会打印 jar 总数、实际存在数与 sources jar 数。没有源码 jar 时降级为
**字节码结构视图**文本，不是反编译。手写等价命令与 PowerShell `-D` 参数引号说明见
`testdata/demo1/classpath.example.txt`。

> 不要在 reactor 根直接跑 `mvn dependency:build-classpath`：该 goal 对每个模块各执行一次，
> 相对 `outputFile` 会按各模块 basedir 解析而散落多份，根目录那份只来自父 pom。

### 扫描排除规则

按**目录名 / 文件名**匹配（不写路径），三种入口叠加：内置目录排除、VS Code 设置
`ld-java-analyze.excludeRules`、`<workspace>/.ld-java-analyze/ignore` 文件。

| 写法 | 含义 |
| --- | --- |
| `# 注释` / 空行 | 忽略 |
| `generated/` 或 `dir:generated` | 排除该**目录名** |
| `dir:build*` | 目录名通配 |
| `file:*.generated.java` | 排除匹配的**文件名** |
| `*Test.java` | 目录名和文件名都匹配 |
| `!file:KeepTest.java` | 反选；**最后一条匹配的规则生效** |

`*` / `?` 通配、大小写不敏感。`.ld-java-analyze` 始终排除且不能反选。
示例见 `testdata/demo1/ignore.example.txt`；`ld-java-analyze: 编辑扫描排除规则`
创建文件，保存后自动重建索引。

### 日志

默认关闭。启用方式任选其一：VS Code 设置 `ld-java-analyze.logDir`、
命令 `ld-java-analyze: 设置日志输出目录`，或 `<workspace>/.ld-java-analyze/config.json`：

```json
{ "logDir": ".ld-java-analyze/logs" }
```

相对路径按工作区根解析。按 UTC 日期分文件 `ld-java-analyze-YYYY-MM-DD.log`，
**最多保留 7 天**，LSP 每次启动自动清理。日志只写文件，绝不写 stdout（会破坏 JSON-RPC）。
记录启动参数、索引统计、解析失败、定义跳转决策链、引用查询、命令执行与关闭。

### 配置速查

| 设置 | 默认 | 说明 |
| --- | --- | --- |
| `ld-java-analyze.serverPath` | `""` | LSP 二进制绝对路径；留空用扩展自带 server |
| `ld-java-analyze.referenceRoots` | `[]` | 引用过滤目录；空 = 整个工作区 |
| `ld-java-analyze.excludeRules` | `[]` | 扫描排除规则 |
| `ld-java-analyze.useBuiltinExcludes` | `true` | 内置目录排除开关 |
| `ld-java-analyze.logDir` | `""` | 日志目录；空 = 不写日志 |

---

## 能力与命令

| 能力 | LSP 方法 |
| --- | --- |
| 跳定义（含 Lombok / record 合成符号）、库方法源码优先 | `textDocument/definition` |
| 查引用（可按目录过滤）、同文件读写高亮 | `textDocument/references` / `documentHighlight` |
| 文件结构、工作区符号、悬停 | `documentSymbol` / `workspace/symbol` / `hover` |
| 跳类型定义、跳实现 | `typeDefinition` / `implementation` |
| MyBatis XML ↔ Mapper 方法双向跳转 | `textDocument/definition`（`Link`） |

自定义命令（`workspace/executeCommand`）：

| 命令 | 说明 |
| --- | --- |
| `ld-java-analyze.reindex` | 增量重建索引并写回快照 |
| `ld-java-analyze.setReferenceRoots` / `referencesInPaths` | 设置过滤目录 / 按目录查引用 |
| `ld-java-analyze.jarCandidates` | 同名类的全部 JAR 候选（含源码与字节码） |
| `ld-java-analyze.setExcludeRules` / `getExcludeRules` | 读写扫描排除规则 |
| `ld-java-analyze.setLogDir` | 运行时切换或关闭日志 |
| `ld-java-analyze.stats` | 索引统计、生效规则、`logDir` / `retentionDays` |

命令面板对应：`在指定目录下查找引用`、`设置引用过滤目录`、`列出同名类所在的 JAR`、
`编辑扫描排除规则`、`设置日志输出目录`、`重建索引`、`查看索引状态`、`自检所有命令`。

目录选择与结果展示用 **QuickPick**，所有结果写入 **「ld-java-analyze」输出面板**；
取消、空结果、服务未就绪、请求失败都有明确提示。

---

## 验证与排障

```powershell
node testdata/verify-lsp.mjs
# 改为验证已安装扩展里的 server
$env:LD_JAVA_ANALYZE_SERVER = "$env:USERPROFILE\.vscode\extensions\local.ld-java-analyze-0.1.0\server\ld-java-analyze.exe"
node testdata/verify-lsp.mjs
```

`testdata/demo1` 是验收工程，覆盖跨文件调用、引用过滤、实现跳转、Lombok、
MyBatis 双向 Link、类型定义与扫描规则边界样例。CI 闸门见 `.github/workflows/ci.yml`
（Rust fmt / clippy / test + TS typecheck + VSIX 打包）。

排障顺序：

1. 命令点击无反应 → `Developer: Reload Window`；再跑 `ld-java-analyze: 自检所有命令`。
2. 索引文件数异常 → `ld-java-analyze: 查看索引状态`，检查 `excludeRules` 与工作区根。
3. 库方法跳不到源码 → 确认 `classpath.txt` 完整且同目录有 `*-sources.jar`，
   然后执行 `ld-java-analyze: 重建索引`。
4. 仍无头绪 → 启用 `logDir`，查看 `.ld-java-analyze/logs/` 的定义跳转决策链。

---

## 项目结构

```
Cargo.toml                   Rust workspace
crates/nav-core              符号 / 出现 / 链接 / 类型边模型，扫描排除规则
crates/nav-java              tree-sitter 解析、Lombok 合成、MyBatis provider
crates/nav-index             增量扫描、跨文件引用绑定、引用倒排、快照
crates/nav-maven             只读 reactor / classpath / sources.jar 发现
crates/nav-jar               多 JAR 候选、源码优先、字节码结构视图
crates/ld-java-analyze       LSP server、命令、日志
editors/vscode               VS Code 薄扩展
scripts/                     classpath 生成脚本（ps1 / sh）
testdata/                    验收工程与端到端冒烟测试
```

依赖单向：`nav-core → nav-java → nav-index → ld-java-analyze → editors/vscode`。
`WorkspaceIndex` 是所有导航能力的唯一真相源，框架 provider 只返回 `Link`；
快照存于 `.ld-java-analyze/cache/`，解析器输出结构变化时 `INDEX_VERSION` 递增使旧快照失效。

技术栈：[`tower-lsp-server`](https://crates.io/crates/tower-lsp-server)、
[`tree-sitter-java`](https://crates.io/crates/tree-sitter-java)、
[`zip`](https://crates.io/crates/zip)、
[`vscode-languageclient`](https://www.npmjs.com/package/vscode-languageclient)、TypeScript。

---

## 已知限制

- 跨文件绑定是启发式（名称 + 类别 + 接收者类型 + 实参个数），未做泛型实参推导、
  参数类型级重载匹配与完整作用域分析。
- Java 语法以 Java 8 为主并最小支持 record；其它 Java 16+ 特性未覆盖。
- 无源码 jar 时是结构视图而非反编译；`Decompiler` trait 已预留。
- 索引快照使用 JSON 而非 `redb`，但快照永远可重建。

---

## 文档

- [`AGENT.md`](AGENT.md) —— 迭代规范、提示信息约定、发布闸门与已知陷阱
- [`editors/vscode/README.md`](editors/vscode/README.md) —— 扩展内说明页
- [`testdata/verify-lsp.mjs`](testdata/verify-lsp.mjs) —— 端到端冒烟测试

## 许可证

[MIT](LICENSE)
