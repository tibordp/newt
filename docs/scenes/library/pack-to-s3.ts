import {
  at,
  dir,
  file,
  fsStats,
  gb,
  mainWindow,
  mb,
  operation,
  pane,
  s3,
} from "../builders";
import { docsColumns } from "../demo";
import type { Scene } from "../types";

const bucket = s3(1, "demo.backup");
const project = "/Users/demo/src/newt";

const scene: Scene = {
  description:
    "Packing a project into a tar.zst archive directly onto S3, mid-flight",
  size: { width: 1200, height: 768 },
  ...docsColumns(1200),
  window: "main",
  state: mainWindow({
    panes: [
      pane({
        path: project,
        focused: "agents",
        gitBranch: "master",
        fsStats: fsStats(gb(994.66), gb(297.71)),
        git: {
          agents: "ignored",
          design_docs: "ignored",
          dist: "ignored",
          node_modules: "ignored",
          target: "ignored",
          "target-agents": "ignored",
        },
        entries: [
          dir("agents", { modified: at("2026-07-17T17:32:43") }),
          dir("design_docs", { modified: at("2026-07-28T10:19:01") }),
          dir("dist", { modified: at("2026-07-27T13:13:14") }),
          dir("docs", { modified: at("2026-07-17T00:58:44") }),
          dir("libs", { modified: at("2026-07-18T23:10:24") }),
          dir("node_modules", { modified: at("2026-07-15T14:28:37") }),
          dir("packaging", { modified: at("2026-07-21T21:55:50") }),
          dir("public", { modified: at("2026-07-24T12:08:08") }),
          dir("scripts", { modified: at("2026-07-14T18:16:35") }),
          dir("src", { modified: at("2026-07-26T20:21:15") }),
          dir("src-tauri", { modified: at("2026-07-28T17:24:38") }),
          dir("target", { modified: at("2026-07-28T17:24:40") }),
          dir("target-agents", { modified: at("2026-07-17T00:57:12") }),
          dir("xtask", { modified: at("2026-07-27T11:02:51") }),
          dir(".git", { modified: at("2026-07-28T17:30:02") }),
          dir(".github", { modified: at("2026-07-12T09:14:40") }),
          file("app-icon.png", {
            size: 124_905,
            modified: at("2026-06-10T11:21:33"),
          }),
          file("Cargo.lock", {
            size: 191_227,
            modified: at("2026-07-28T16:52:10"),
          }),
          file("Cargo.toml", {
            size: 536,
            modified: at("2026-07-18T22:47:55"),
          }),
          file("CLAUDE.md", {
            size: 13_676,
            modified: at("2026-07-27T13:36:02"),
          }),
          file("CONTRIBUTING.md", {
            size: 1_777,
            modified: at("2026-07-27T13:36:36"),
          }),
          file("eslint.config.js", {
            size: 863,
            modified: at("2026-03-01T00:00:42"),
          }),
          file("FEATURE_DUMP.md", {
            size: 178_332,
            modified: at("2026-07-28T15:46:06"),
          }),
          file("index.html", {
            size: 983,
            modified: at("2026-05-08T15:51:10"),
          }),
          file("LICENSE", {
            size: 35_149,
            modified: at("2026-07-16T13:10:09"),
          }),
          file("Makefile", {
            size: 1_637,
            modified: at("2026-07-27T12:39:23"),
          }),
          file("package-lock.json", {
            size: 132_557,
            modified: at("2026-07-15T01:10:33"),
          }),
          file("package.json", {
            size: 1_941,
            modified: at("2026-07-16T13:10:58"),
          }),
          file("README.md", {
            size: 4_696,
            modified: at("2026-07-27T12:41:41"),
          }),
          file("THIRD-PARTY-NOTICES.md", {
            size: 112_068,
            modified: at("2026-07-28T12:25:41"),
          }),
          file("TODO.md", {
            size: 11_441,
            modified: at("2026-07-28T14:33:11"),
          }),
          file(".gitignore", {
            size: 402,
            modified: at("2026-07-02T10:11:12"),
          }),
        ],
      }),
      pane({
        path: "/newt",
        vfs: bucket,
        entries: [
          ["13", 36_902_411, "2026-07-16T13:04:36"],
          ["14", 36_183_492, "2026-07-16T13:03:22"],
          ["15", 36_688_109, "2026-07-16T13:03:35"],
          ["16", 37_975_807, "2026-07-17T00:21:42"],
        ]
          .map(([day, size, modified]) =>
            file(`newt-2026-07-${day}.tar.zst`, {
              size: size as number,
              modified: at(modified as string),
              user: null,
              group: null,
              mode: null,
            }),
          )
          .concat([
            file("newt-agents-2026-07-17.tar.zst", {
              size: 34_500_976,
              modified: at("2026-07-17T00:57:26"),
              user: null,
              group: null,
              mode: null,
            }),
          ]),
      }),
    ],
    operations: [
      operation({
        id: 1,
        kind: "Pack",
        description: `Packing 1 item(s) to s3://demo.backup/newt/newt-agents-2026-07-28.tar.zst`,
        total_bytes: mb(108.62),
        bytes_done: mb(26.61),
        total_items: 14,
        items_done: 3,
        current_item: "x86_64-unknown-linux-musl/newt-agent",
      }),
    ],
  }),
};

export default scene;
