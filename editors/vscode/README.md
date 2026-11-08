# ld-java-analyze

Java 8 / Maven 代码导航扩展：VS Code 薄客户端 + 随扩展附带的原生 Rust LSP server。

**能力**：定义跳转、引用查找（可按目录过滤）、读写高亮、文件/工作区符号、跳类型定义、
跳实现、悬停、MyBatis XML ↔ Mapper 方法双向跳转、Lombok / record 合成符号、
库方法源码优先与多 JAR 候选。

## 使用

1. 把**被索引的项目根**作为 VS Code 工作区打开（多模块项目打开最外层带 `pom.xml` 的目录）。
2. 用 Maven 自行生成 `<工作区根>/.ld-java-analyze/classpath.txt` 与 `*-sources.jar`。
   扩展不启动 Maven、不联网、不下载源码；**多模块项目必须用 `-pl` + 绝对路径**，
   完整命令见项目根 `README.md` 的「配置被索引的项目」。
3. 打开任意 `.java` / `.xml` 文件即激活。命令面板跑
   `ld-java-analyze: 自检所有命令` 与 `ld-java-analyze: 查看索引状态` 确认索引与 classpath 已加载。

所有运行期状态只写 `<工作区根>/.ld-java-analyze/`，日志默认关闭。

## 命令

`在指定目录下查找引用`（QuickPick 选目录 → 结果列表跳转）、`设置引用过滤目录`、
`列出同名类所在的 JAR`、`编辑扫描排除规则`、`设置日志输出目录`、`重建索引`、
`查看索引状态`、`自检所有命令`。

结果同时写入 **「ld-java-analyze」输出面板**；取消、空结果、失败都有明确提示。

## 设置

| 设置 | 默认 | 说明 |
| --- | --- | --- |
| `ld-java-analyze.serverPath` | `""` | LSP 二进制绝对路径；留空用扩展自带 server |
| `ld-java-analyze.referenceRoots` | `[]` | 引用过滤目录；空 = 整个工作区 |
| `ld-java-analyze.excludeRules` | `[]` | 按目录名/文件名匹配的扫描排除规则 |
| `ld-java-analyze.useBuiltinExcludes` | `true` | 内置目录排除开关 |
| `ld-java-analyze.logDir` | `""` | 日志目录；按天分文件、保留 7 天、启动清理 |

## 排障

命令无反应先执行 `Developer: Reload Window`，再跑 `自检所有命令`；
索引或跳转异常看 `<工作区根>/.ld-java-analyze/logs/` 与输出面板。

完整的**构建 → 调整插件 → 打包安装 → 使用验证**流程见项目根 `README.md`，
迭代规范见 `AGENT.md`。
