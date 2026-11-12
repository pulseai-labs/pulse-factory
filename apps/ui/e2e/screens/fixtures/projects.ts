// The d25 test data of the Projects page. Owner: w5 (its `folder_list` answer goes in `verbs`).
import type { ProjectList } from "@pulse-factory/api-client";
import { project } from "../../../src/test/fake-transport";
import type { Verbs } from "./verbs";

export const PROJECTS: ProjectList = {
  schema_version: 1,
  projects: [
    { ...project(1, "pulse-factory"), created_at: "2026-11-08T09:00:00Z" },
    { ...project(2, "pilot-fourseat"), created_at: "2026-11-08T09:02:00Z" },
  ],
};

export const verbs: Verbs = {
  project_list: () => PROJECTS,
};
