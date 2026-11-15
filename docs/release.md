# 一 PR 一版本的自动发布

## 合并前

版本在 PR 中统一更新 `package.json`、`src-tauri/tauri.conf.json`、Cargo package、Cargo.lock 根包和 README 当前版本。`pnpm version:check` 检查一致性；PR 和 `merge_group` 的 Version job 读取实时默认分支的不可变快照、最新 Release 与精确标签，要求版本严格增加且未被使用。数字段不超过 `255.255.65535`；允许 `-beta<N>` 单段预发布后缀，其余 semver 后缀（rc、多段）仍然拒绝——MSI 与 MSIX 这两种 Windows 安装包格式只接受纯数字版本号，预发布版本跳过这两个产物。

CI 在 PR opened/synchronize/reopened/ready_for_review 和 merge_group checks_requested 执行，只使用只读令牌，不给 PR 注入签名证书、写令牌或 attestation 权限。质量检查包括前端/发布脚本回归测试、格式、版本、IPC、Rust 测试/Clippy、依赖审计、Windows 生产构建和五种产物验证。工作流**没有 `schedule` 触发**：CI 只在 PR、merge queue 与手动 `workflow_dispatch` 时运行，不会每个周一自动跑一次无人处理的定时任务；`scripts/workflows.test.mjs` 断言 `ci.on` 只有这三个事件，重新加回 cron 会让测试失败。

## 管理员必须设置的仓库规则

本次实际登录的 GitHub Settings 页面显示 **You don't have access to repository options**；API 也没有 admin 权限。以下规则没有被自动配置，不能把工作流存在等同于分支保护已生效。

在 Settings → Rules → Rulesets，为默认分支设置 active 规则（或等价分支保护）：

1. Require a pull request before merging。
2. Require status checks to pass，并要求分支基于最新 base，选择三个检查：
   - `Version consistency and uniqueness`
   - `Audit JavaScript and Rust dependencies`
   - `Validate and package Windows x64`
3. 如果启用 Merge Queue，**Maximum pull requests to merge 必须是 1**；required checks 已支持 `merge_group`。不要把多个各自携带版本号的 PR 一次合入。
4. 不设置允许绕过上述版本/质量门禁的自动发布路径。Actions 必须允许仓库中的固定 SHA actions；Release job 必须能请求 contents:write、id-token:write、attestations:write。

没有这些保护时，拥有直接写权限的用户仍可能合入重复版本；工作流会拒绝抢占现有标签，但不能替代仓库合并规则。

## 推送与受信任发布

`release.yml` 由 `push` 到默认分支（外加手动 `workflow_dispatch` 用于重发）触发，分为三个串行 job（步骤名均为中文）：

1. **确定发布类型与标签**（ubuntu）：验证发布来源为默认分支 HEAD，读取版本判定发布类型（纯数字 → 正式 Release，`x.y.z-beta<N>` → Pre-Release，输出为 `channel`），做幂等门控并创建标签；已发布版本在此直接标记 `published`，后续 job 跳过。
2. **构建可执行文件**（windows）：构建、打包（预发布跳过 MSI/MSIX）、签名、产物校验，产物经 artifact 传给发布 job。
3. **发布**（ubuntu）：下载产物，生成来源证明与 attestation，创建并发布 Release、核对上传摘要。旧的「合并通知 → `workflow_run`」两阶段链已移除：push 事件只有具备仓库写权限的账号才能在上游触发，fork 无法伪装，间接身份验证不再必要。

只读 resolve job 从本次 workflow 的不可变定义加载校验器（`scripts/release-identity.mjs`），把默认分支当前 HEAD 确定为发布源并输出其 SHA；构建源码始终 checkout 该 SHA，不使用 run 的 `github.sha` 推断。排队期间 HEAD 前进时按最新 HEAD 发布：版本号在 checkout 后由 `release-state.mjs` 重新读取并做幂等门控，已发布版本不会被重复构建或覆盖。

版本类型在发布时自动判定，与 Rust 更新检查的渠道规则同源：`tauri.conf.json` 的版本去掉 `v` 前缀后含 `-` 即 Pre-Release（`gh`/API 设置 `prerelease: true` 且永不抢占 latest），纯数字版本发正式 Release 并按 semver 与现有 latest 比较决定是否标记 latest。

## 排队、幂等与恢复

全仓库共享 `floepod-release` concurrency group，`cancel-in-progress: false`、`queue: max`。GitHub 当前最多保存 100 个 pending，按进入队列顺序处理；不是无限队列，也不保证所有 runner 的调度顺序等于 PR merge 时间。旧版本后发布时不会错误替代较新 Latest Release。[当前并发队列语法](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#concurrency)。

标签只允许不存在时创建，或已存在且最终指向同一个精确 SHA；不移动已占用标签。公开 Release 重跑时验证来源标记和全部附件摘要，成功即结束。中断的 draft 可以重新构建并覆盖 draft 附件，但公开附件不覆盖。上传后核对七个文件的数量、大小、uploaded 状态和 GitHub SHA-256 digest，全部符合才发布 draft，再次验证远端资产。

恢复方式：重跑失败的 Release run；或从默认分支手动触发 Release（`workflow_dispatch`）。手动触发与 push 同样以默认分支当前 HEAD 为发布源，不能输入 PR head、任意 branch 或任意 SHA 绕过来源验证。100 pending 上限溢出等 GitHub 外部失败需要等下一次 push 或再次手动派发；不会谎报已发布。

## 打包、签名与来源证明

合并后不重复执行 lint/test/audit/契约检查；只验证源码身份、版本和发布状态，生产构建、打包、签名、校验、证明和发布。

先构建 EXE，有证书时通过 Tauri `signCommand` 回调签署各包修改了包类型标记后的程序，以及安装器和卸载器。Tauri 打包结束会恢复原始 EXE，因此随后再签署裸 EXE，用它制作 MSIX 和 ZIP；最终逐一验证签名，核对 ZIP/MSIX 内程序与裸 EXE 的摘要。预发布版本只打 NSIS 与便携包（MSI/MSIX 的版本号约束见上），产物校验按版本动态要求三件或五件套。MSIX Publisher 从证书主题取得。签名使用现有两个 secrets：`WINDOWS_SIGN_CERTIFICATE_BASE64`、`WINDOWS_SIGN_CERTIFICATE_PASSWORD`。缺少证书允许 unsigned 发布，manifest 和 Release Notes 明确标注；有证书却签名失败会终止。当前本地验证没有生产证书，不声称已完成真实签名验证。[Tauri 自定义签名命令](https://v2.tauri.app/distribute/sign/windows/#custom-sign-commands)。

最终产物（正式版五件、预发布三件）加 `release-manifest.json`、`SHA256SUMS.txt` 统一生成 attestation。自定义 SLSA v1 predicate 的 `buildType` 必须使用服务端为该 predicate 类型唯一放行的官方 `https://actions.github.io/buildtypes/workflow/v1`，自造 buildType 会在上传时被以 unsupported build type 拒绝（[actions/attest#195](https://github.com/actions/attest/issues/195)）；服务端还会按该 buildType 的格式校验 predicate——`externalParameters.workflow.ref` 等 ref 类字段必须等于本次 run 的 git ref（如 `refs/heads/main`），commit SHA 只能出现在 `digest.gitCommit`，`internalParameters.github` 必须包含 run 的 OIDC 身份字段，否则上传被以 values do not match / required predicate value is unset 拒绝。predicate 因此由 job 的 OIDC token claims 按官方生成器的形状构造，本次实际构建的 immutable source SHA 记录在 externalParameters.source 与 `digest.gitCommit`，执行 workflow 的定义 SHA 记录在 `internalParameters`，与排队期间可能前进的 run SHA 分开记录。来源证明签署最终字节，不把 Authenticode 未签名描述成已签名。普通 tag push 与其他分支的 push 不触发发布；推送到 main 即按当前版本号发布（已发布版本幂等跳过）。

工作流使用固定完整 action SHA。`contracts/github-workflow.schema.json` 是 2026-09-08 从 SchemaStore 取得的快照，用于离线 PR 校验，已包含 queue:max；actionlint 1.7.12 尚未包含这个新字段，其唯一 queue 告警不代表 GitHub 当前语法不支持。其他语法和 action 引用仍由测试检查。
