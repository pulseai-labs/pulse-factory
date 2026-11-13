// The d25 test data of the Projects page. Owner: w5 (its `folder_list` answer goes in `verbs`).
import type { FolderList, ProjectList } from "@pulse-factory/api-client";
import { folderEntry, project } from "../../../src/test/fake-transport";
import type { Verbs } from "./verbs";

export const PROJECTS: ProjectList = {
  schema_version: 1,
  projects: [
    { ...project(1, "pulse-factory"), created_at: "2026-11-08T09:00:00Z" },
    { ...project(2, "pilot-fourseat"), created_at: "2026-11-08T09:02:00Z" },
  ],
};

/**
 * The home folder of the fixtures' server, `/home/dev`, the root of the projects above: what the Add a
 * project dialog lists (11f). `pilot-fourseat` and `pulse-factory` are projects already, so their rows say
 * so; `scratch` is a plain folder, so picking it shows the `git init` tip.
 */
export const FOLDERS: FolderList = {
  schema_version: 1,
  home: "/home/dev",
  path: "/home/dev",
  parent: null,
  repo: null,
  entries: [
    folderEntry("notes"),
    folderEntry("pilot-fourseat", { git: true, branch: "main", project_id: "P-2" }),
    folderEntry("pulse-factory", { git: true, branch: "main", project_id: "P-1" }),
    folderEntry("pulse-trader", { git: true, branch: "main" }),
    folderEntry("scratch"),
  ],
  truncated: false,
};

// The module answers no `project_add` and no `project_remove`: the screenshots show no submit.
export const verbs: Verbs = {
  project_list: () => PROJECTS,
  folder_list: () => FOLDERS,
};
