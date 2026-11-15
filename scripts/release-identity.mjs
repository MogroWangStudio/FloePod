import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { githubApi, output } from "./github-api.mjs";

// push / workflow_dispatch 都以默认分支当前 HEAD 为发布源。写入默认分支
// 本身就需要仓库写权限（fork 无法在上游触发 push 事件），因此不再需要旧
// 「合并通知 → workflow_run」两阶段链的间接身份验证。排队期间 HEAD 前进
// 时按最新 HEAD 发布：版本号在 checkout 后由 release-state 重新读取，幂等
// 门控保证已发布版本不会被重复构建。
export async function resolveSource(api) {
  const repository = await api("");
  const branch = repository.default_branch;
  const head = await api(`/commits/${encodeURIComponent(branch)}`);
  if (!/^[0-9a-f]{40}$/.test(head.sha ?? "")) throw Error("Invalid default branch HEAD");
  return { sha: head.sha, branch };
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  if (!["push", "workflow_dispatch"].includes(process.env.GITHUB_EVENT_NAME))
    throw Error("Unsupported release trigger");
  const identity = await resolveSource(githubApi());
  output(identity);
  console.log(`Verified default branch ${identity.branch} at ${identity.sha}`);
}
