import assert from "node:assert/strict";
import test from "node:test";
import { resolveSource } from "./release-identity.mjs";

test("release source is always the current default branch HEAD", async () => {
  const sha = "a".repeat(40);
  const data = {
    "": { id: 1, default_branch: "main" },
    "/commits/main": { sha },
  };
  assert.deepEqual(await resolveSource(async (path) => data[path]), { sha, branch: "main" });

  const other = {
    "": { id: 1, default_branch: "trunk" },
    "/commits/trunk": { sha: "b".repeat(40) },
  };
  assert.deepEqual(await resolveSource(async (path) => other[path]), {
    sha: "b".repeat(40),
    branch: "trunk",
  });
});

test("malformed default branch HEAD is rejected", async () => {
  const data = {
    "": { id: 1, default_branch: "main" },
    "/commits/main": { sha: "not-a-sha" },
  };
  await assert.rejects(resolveSource(async (path) => data[path]));
  await assert.rejects(resolveSource(async () => ({})));
});
