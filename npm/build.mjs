#!/usr/bin/env node
// Assemble the npm packages for a released rsntr version.
//
//   node npm/build.mjs --version 0.1.0
//   node npm/build.mjs --version 0.1.0 --assets ./local-dir
//
// Downloads (or reads) the GitHub release archives, verifies each against
// its published .sha256, and writes ready-to-publish packages into
// npm/dist/: one per platform holding the binary, plus the root `rsntr`
// package that selects between them.
//
// The binaries are bundled into the packages rather than fetched by a
// postinstall script. Postinstall downloads are the usual workaround for
// large binaries, but they fail in offline and network-restricted sandboxes
// -- which is exactly where LLM agents run, and the reason this channel
// exists. At ~18MB per platform, bundling is affordable.

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdirSync, rmSync, readFileSync, writeFileSync, copyFileSync, existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = "Resonator-LLC/rsntr";

// node platform-arch -> the rust target that serves it. The Linux binaries
// are glibc builds (2.35 floor, built on ubuntu-22.04): musl's 4-aligned
// cmsghdr trips noq-udp's cmsg alignment assert on the first received
// packet, so static musl builds abort on any real network traffic.
const TARGETS = [
  { key: "linux-x64", os: "linux", cpu: "x64", target: "x86_64-unknown-linux-gnu", archive: "tar.gz" },
  { key: "linux-arm64", os: "linux", cpu: "arm64", target: "aarch64-unknown-linux-gnu", archive: "tar.gz" },
  { key: "darwin-x64", os: "darwin", cpu: "x64", target: "x86_64-apple-darwin", archive: "tar.gz" },
  { key: "darwin-arm64", os: "darwin", cpu: "arm64", target: "aarch64-apple-darwin", archive: "tar.gz" },
  { key: "win32-x64", os: "win32", cpu: "x64", target: "x86_64-pc-windows-msvc", archive: "zip" },
];

function arg(name) {
  const i = process.argv.indexOf(`--${name}`);
  return i === -1 ? undefined : process.argv[i + 1];
}

const version = arg("version");
if (!version) {
  console.error("usage: node npm/build.mjs --version <x.y.z> [--assets <dir>]");
  process.exit(1);
}

const assetsDir = arg("assets") ? resolve(arg("assets")) : join(HERE, "assets");
const distDir = join(HERE, "dist");
const download = !arg("assets");

rmSync(distDir, { recursive: true, force: true });
mkdirSync(distDir, { recursive: true });
mkdirSync(assetsDir, { recursive: true });

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

// Extraction shells out: node has no archive support, and the alternative
// is vendoring a tar/zip implementation for a build script.
function extract(archivePath, into, kind) {
  mkdirSync(into, { recursive: true });
  if (kind === "tar.gz") {
    execFileSync("tar", ["xzf", archivePath, "-C", into]);
  } else {
    try {
      execFileSync("unzip", ["-oq", archivePath, "-d", into]);
    } catch {
      // unzip is absent on some minimal images; python3 is not.
      execFileSync("python3", ["-m", "zipfile", "-e", archivePath, into]);
    }
  }
}

console.log(`Building npm packages for rsntr ${version}`);

for (const t of TARGETS) {
  const stem = `rsntr-${version}-${t.target}`;
  const archiveName = `${stem}.${t.archive}`;
  const archivePath = join(assetsDir, archiveName);
  const sumPath = `${archivePath}.sha256`;

  if (download && !existsSync(archivePath)) {
    console.log(`  fetching ${archiveName}`);
    execFileSync(
      "gh",
      ["release", "download", `v${version}`, "--repo", REPO, "--pattern", `${archiveName}*`, "--dir", assetsDir],
      { stdio: "inherit" }
    );
  }

  if (!existsSync(archivePath)) throw new Error(`missing asset: ${archivePath}`);
  if (!existsSync(sumPath)) throw new Error(`missing checksum: ${sumPath}`);

  // Verify before unpacking, not after. A corrupted or substituted archive
  // should never reach the extraction step, let alone a published package.
  const expected = readFileSync(sumPath, "utf8").trim().split(/\s+/)[0];
  const actual = sha256(archivePath);
  if (expected !== actual) {
    throw new Error(`checksum mismatch for ${archiveName}\n  expected ${expected}\n  actual   ${actual}`);
  }
  console.log(`  ${t.key}: checksum ok`);

  const work = join(distDir, `.unpack-${t.key}`);
  extract(archivePath, work, t.archive);

  const exe = t.os === "win32" ? "rsntr.exe" : "rsntr";
  const binarySrc = join(work, stem, exe);
  if (!existsSync(binarySrc)) throw new Error(`binary not found in archive: ${binarySrc}`);

  const pkgName = `rsntr-${t.key}`;
  const pkgDir = join(distDir, pkgName);
  mkdirSync(join(pkgDir, "bin"), { recursive: true });
  copyFileSync(binarySrc, join(pkgDir, "bin", exe));
  // copyFileSync does not carry the executable bit on every platform.
  if (t.os !== "win32") execFileSync("chmod", ["+x", join(pkgDir, "bin", exe)]);

  // os/cpu are what make this work: npm refuses to install a platform
  // package on a host it does not match, so the optionalDependency set in
  // the root package resolves to exactly one download.
  writeFileSync(
    join(pkgDir, "package.json"),
    JSON.stringify(
      {
        name: pkgName,
        version,
        description: `rsntr binary for ${t.os} ${t.cpu}`,
        homepage: `https://github.com/${REPO}`,
        repository: { type: "git", url: `git+https://github.com/${REPO}.git` },
        license: "MIT OR Apache-2.0",
        os: [t.os],
        cpu: [t.cpu],
        files: ["bin/"],
      },
      null,
      2
    ) + "\n"
  );

  rmSync(work, { recursive: true, force: true });
  console.log(`  ${t.key}: packaged as ${pkgName}@${version}`);
}

// Root package: same source as the checked-in template, with the version
// placeholders filled in so the optional deps pin this exact release.
const rootSrc = join(HERE, "rsntr");
const rootDir = join(distDir, "rsntr");
mkdirSync(join(rootDir, "bin"), { recursive: true });
copyFileSync(join(rootSrc, "bin", "rsntr"), join(rootDir, "bin", "rsntr"));
execFileSync("chmod", ["+x", join(rootDir, "bin", "rsntr")]);

const rootPkg = JSON.parse(readFileSync(join(rootSrc, "package.json"), "utf8"));
rootPkg.version = version;
for (const t of TARGETS) rootPkg.optionalDependencies[`rsntr-${t.key}`] = version;
writeFileSync(join(rootDir, "package.json"), JSON.stringify(rootPkg, null, 2) + "\n");

for (const f of ["README.md", "LICENSE-MIT", "LICENSE-APACHE"]) {
  const src = join(HERE, "..", f);
  if (existsSync(src)) copyFileSync(src, join(rootDir, f));
}
if (!rootPkg.files.includes("README.md"))
  rootPkg.files.push("README.md", "LICENSE-MIT", "LICENSE-APACHE");
writeFileSync(join(rootDir, "package.json"), JSON.stringify(rootPkg, null, 2) + "\n");

console.log(`\nWrote ${TARGETS.length + 1} packages to npm/dist/`);
console.log("Publish order matters: platform packages first, root last, so");
console.log("`npm install rsntr` never resolves before its binaries exist.");
