import assert from "node:assert/strict";
import test from "node:test";
import {
  ensureTag,
  validateRelease,
  verifyRemote,
  expectedAssetCount,
  verifyTagOwnership,
} from "./release-state.mjs";
test("pre-release channel ships fewer packages than stable", () => {
  // beta：裸 exe + NSIS + 便携包 + manifest + 校验和；正式版另有 MSI/MSIX。
  assert.equal(expectedAssetCount("2.0.0-beta1"), 5);
  assert.equal(expectedAssetCount("2.0.0"), 7);
});
test("tag ownership check is read-only and rejects foreign commits", async () => {
  const sha = "a".repeat(40);
  // 标签不存在：通过（不创建）
  await verifyTagOwnership(async () => null, "v2.0.0-beta5", sha);
  // 指向同一提交：通过
  await verifyTagOwnership(
    async (path) => ({ object: { type: "commit", sha } }),
    "v2.0.0-beta5",
    sha,
  );
  // 指向其他提交：拒绝
  await assert.rejects(
    verifyTagOwnership(
      async () => ({ object: { type: "commit", sha: "b".repeat(40) } }),
      "v2.0.0-beta5",
      sha,
    ),
  );
  // annotated tag 解引用后再比对
  await verifyTagOwnership(
    async (path) =>
      path.includes("/git/tags/")
        ? { object: { type: "commit", sha } }
        : { object: { type: "tag", sha: "c".repeat(40) } },
    "v2.0.0-beta5",
    sha,
  );
});
test("tag retries are idempotent and never move another commit's tag", async () => {
  const sha = "a".repeat(40),
    calls = [];
  await ensureTag(
    async (path, options) => {
      calls.push([path, options]);
      return null;
    },
    "v1.6.1",
    sha,
  );
  assert.equal(calls[1][1].body.sha, sha);
  await ensureTag(async () => ({ object: { type: "commit", sha } }), "v1.6.1", sha);
  await assert.rejects(
    ensureTag(async () => ({ object: { type: "commit", sha: "b".repeat(40) } }), "v1.6.1", sha),
  );
});
test("release retries require original provenance and complete matching assets", async () => {
  assert.throws(() => validateRelease({ tag_name: "v1.0.0", body: "foreign" }, "v1.0.0", "abc"));
  const expected = [{ name: "app.exe", size: 10, sha256: "abc" }];
  const asset = { name: "app.exe", size: 10, state: "uploaded", digest: "sha256:abc" };
  await verifyRemote(async () => [asset], { id: 1 }, expected);
  for (const assets of [
    [],
    [asset, asset],
    [{ ...asset, state: "starter" }],
    [{ ...asset, digest: "sha256:other" }],
  ])
    await assert.rejects(verifyRemote(async () => assets, { id: 1 }, expected));
});
