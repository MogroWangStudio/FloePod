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
  // 版本类型判定：纯数字版本发正式 Release，带后缀发 Pre-Release。
  const releaseCommands = Object.values(release.jobs)
    .flatMap((job) => job.steps ?? [])
    .map((step) => step.run ?? "")
    .join("\n");
  assert.doesNotMatch(
    releaseCommands,
    /cargo (test|clippy|fmt|audit)|pnpm (test|audit|format:check|ipc:check)/,
  );
  assert.equal(release.jobs.publish.steps[0].with.ref, "${{ needs.resolve.outputs.sha }}");
  assert.equal(release.jobs.publish.steps[0].with["persist-credentials"], false);
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
