import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import test from "node:test";
import YAML from "yaml";
import Ajv from "ajv";

// SchemaStore github-workflow.json, retrieved 2026-09-08. Kept locally so fork
// PRs validate the same schema without network access or mutable CI tooling.
const schema = JSON.parse(
  readFileSync(new URL("../contracts/github-workflow.schema.json", import.meta.url), "utf8"),
);
const validate = new Ajv({ strict: false, allErrors: true, validateFormats: false }).compile(
  schema,
);
const workflow = (name) =>
  YAML.parse(readFileSync(new URL(`../.github/workflows/${name}.yml`, import.meta.url), "utf8"));
test("all Actions workflows conform to the pinned current schema and immutable action references", () => {
  for (const name of readdirSync(new URL("../.github/workflows", import.meta.url))) {
    if (!name.endsWith(".yml")) continue;
    const value = workflow(name.slice(0, -4));
    assert.ok(validate(value), `${name}: ${JSON.stringify(validate.errors)}`);
    for (const job of Object.values(value.jobs))
      for (const step of job.steps ?? []) {
        if (step.uses && !step.uses.startsWith("./"))
          assert.match(step.uses, /^[\w./-]+@[0-9a-f]{40}$/);
      }
  }
});
test("release publishes on push to the default branch without the merge notification chain", () => {
  const release = workflow("release"),
    ci = workflow("ci");
  assert.deepEqual(release.concurrency, {
    group: "floepod-release",
    "cancel-in-progress": false,
    queue: "max",
  });
  // 推送到默认分支即自动发布；不接收任意分支/tag，也不再有合并通知链。
  assert.deepEqual(release.on.push, { branches: ["main"] });
  assert.ok("workflow_dispatch" in release.on);
  assert.equal(release.on.workflow_run, undefined);
  assert.equal(
    readdirSync(new URL("../.github/workflows", import.meta.url))
      .filter((name) => name.endsWith(".yml"))
      .includes("merge-notification.yml"),
    false,
  );
  // 三段式：确定发布类型与标签 -> 构建可执行文件 -> 发布；步骤名用中文。
  assert.deepEqual(Object.keys(release.jobs), ["prepare", "build", "publish"]);
  assert.match(release.jobs.prepare.name, /确定发布类型与标签/);
  assert.match(release.jobs.build.name, /构建可执行文件/);
  assert.match(release.jobs.publish.name, /发布/);
  const stepNames = (job) => (job.steps ?? []).map((step) => step.name);
  for (const job of Object.values(release.jobs))
    for (const name of stepNames(job)) assert.match(name, /[\u4e00-\u9fff]/, name);
  assert.deepEqual(release.jobs.build.needs, ["prepare"]);
  assert.deepEqual(release.jobs.publish.needs, ["prepare", "build"]);
  // 已发布版本（published=true）时后两个 job 直接跳过。
  assert.match(release.jobs.build.if, /published != 'true'/);
  assert.match(release.jobs.publish.if, /published != 'true'/);
  // 第一步只有读权限：标签由发布阶段（contents:write）创建，构建失败的
  // run 不再占用标签。
  assert.deepEqual(release.jobs.prepare.permissions, { contents: "read" });
  // 输出链锁定：--prepare 必须输出 sha（曾因缺失导致 provenance 校验失败），
  // 构建/发布 job 检出与记录都取自同一输出。
  assert.equal(release.jobs.prepare.outputs.sha, "${{ steps.prepare.outputs.sha }}");
  const stateScript = readFileSync(
    new URL("../scripts/release-state.mjs", import.meta.url),
    "utf8",
  );
  assert.match(stateScript, /output\(\{[\s\S]*?\n\s*sha,/);
  for (const jobName of ["build", "publish"]) {
    const job = release.jobs[jobName];
    const checkout = job.steps.find((step) => step.uses?.startsWith("actions/checkout"));
    assert.equal(checkout.with.ref, "${{ needs.prepare.outputs.sha }}", jobName);
    assert.equal(job.env.RELEASE_SHA, "${{ needs.prepare.outputs.sha }}", jobName);
  }
  const prepareRuns = (release.jobs.prepare.steps ?? []).map((step) => step.run ?? "").join("\n");
  assert.match(prepareRuns, /release-state\.mjs --prepare/);
  assert.doesNotMatch(prepareRuns, /--publish/);
  // 构建产物经 artifact 传给发布 job，发布 job 有 OIDC 与 attest 权限。
  assert.deepEqual(release.jobs.publish.permissions, {
    contents: "write",
    "id-token": "write",
    attestations: "write",
  });
  const releaseCommands = Object.values(release.jobs)
    .flatMap((job) => job.steps ?? [])
    .map((step) => step.run ?? "")
    .join("\n");
  assert.doesNotMatch(
    releaseCommands,
    /cargo (test|clippy|fmt|audit)|pnpm (test|audit|format:check|ipc:check)/,
  );
  const checkout = release.jobs.publish.steps.find((step) =>
    step.uses?.startsWith("actions/checkout"),
  );
  assert.equal(checkout.with.ref, "${{ needs.prepare.outputs.sha }}");
  assert.equal(checkout.with["persist-credentials"], false);
});
test("ci stays limited to PR, merge queue and manual triggers", () => {
  const ci = workflow("ci");
  assert.ok(ci.on.merge_group);
  assert.deepEqual(ci.permissions, { contents: "read" });
  for (const job of Object.values(ci.jobs)) assert.equal(job.permissions, undefined);
  // No scheduled runs: a weekly cron produced red runs nobody acted on, and
  // advisory discovery belongs to Dependabot alerts, not to a failing workflow.
  assert.deepEqual(Object.keys(ci.on).sort(), ["merge_group", "pull_request", "workflow_dispatch"]);
  assert.equal(ci.on.push, undefined);
  for (const job of Object.values(ci.jobs)) if (job.if) assert.doesNotMatch(job.if, /schedule/);
});
