// The Add a project dialog (11f, `r1.s5.w5`). A modal over the Projects page: the host (the one server),
// then how. Browse folder lists the folders of the server's home folder with `folder_list`, marks the git
// repositories with their branch, marks a folder that is a project already, and names the new project after
// the folder. A folder that is not inside a repository can get `git init` when it is added, if the
// operator ticks the box. Clone from URL, Create new project and Other hosts are `NotBuilt`.
//
// The dialog calls `folder_list` when it opens and once for each navigation (a row's open button, a
// crumb, the up button). It sets no timer and never polls.
import type { FolderEntry, FolderList, ProjectRecord } from "@pulse-factory/api-client";
import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { asTransportError, useConnection, useServerName } from "../state/connection";
import { Button, Icon, IconButton, Input, NotBuilt } from "../ui";
import "../styles/add-project.css";
import { useCommandId } from "./command-id";

/** What the success view shows. `branch` is read once from the parent folder, after a `git init`. */
interface Added {
  project: ProjectRecord;
  root: string;
  initialised: boolean;
  branch: string | null;
}

export interface AddProjectProps {
  /** Called once after an add, so the page loads `project_list` again. */
  onAdded: () => Promise<void> | void;
  /** Closes the dialog. `openId` is the project that "Open project" names. */
  onClose: (openId?: string) => void;
}

/** Joins the class names that are set. */
const cx = (...parts: Array<string | false>) => parts.filter((p) => p !== false && p !== "").join(" ");

/** One part of the folder path (11f). The home part is "~" with the house icon; its `path` is the full home path. */
interface Crumb {
  label: string;
  path: string;
  home: boolean;
}

/** The crumbs of a path: the home folder first ("~"), then each folder below it. */
function crumbsOf(list: FolderList): Crumb[] {
  const rest = list.path === list.home ? [] : list.path.slice(list.home.length).split("/").filter((s) => s !== "");
  const crumbs: Crumb[] = [{ label: "~", path: list.home, home: true }];
  rest.forEach((segment, i) => {
    crumbs.push({ label: segment, path: `${list.home}/${rest.slice(0, i + 1).join("/")}`, home: false });
  });
  return crumbs;
}

export function AddProject(props: AddProjectProps) {
  const { onAdded, onClose } = props;
  const { call, link } = useConnection();
  const serverName = useServerName();
  const down = link.state === "down";

  const [listing, setListing] = useState<FolderList | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [picked, setPicked] = useState<FolderEntry | null>(null);
  const [name, setName] = useState("");
  const [gitInit, setGitInit] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [added, setAdded] = useState<Added | null>(null);
  const commandId = useCommandId();
  const panel = useRef<HTMLDivElement>(null);
  const live = useRef(true);
  // `call` can change its identity when the link changes; the dialog's calls follow the navigation only.
  const callRef = useRef(call);
  callRef.current = call;

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  /** One `folder_list` call: the home folder with no path, or the folder at `path`. */
  const list = useCallback(async (path?: string) => {
    setListError(null);
    try {
      const answer = await callRef.current((t) => t.folderList(path === undefined ? undefined : { path }));
      if (!live.current) return;
      setListing(answer);
      setPicked(null);
      setName("");
      setGitInit(false);
      setFormError(null);
    } catch (e) {
      if (live.current) setListError(asTransportError(e).message);
    }
  }, []);

  // On opening: the home folder, once.
  useEffect(() => {
    void list();
  }, [list]);

  // On opening: focus goes to the first control.
  useEffect(() => {
    panel.current?.querySelector<HTMLElement>("button:not(:disabled), input:not(:disabled)")?.focus();
  }, []);

  // Escape closes. The listener holds the latest `onClose`, so a re-render does not move focus.
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") closeRef.current();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const pick = (entry: FolderEntry) => {
    setPicked(entry);
    setName(entry.name);
    setGitInit(false);
    setFormError(null);
  };

  // The tip is for a picked folder that is not a repository and not inside one (`repo` is the
  // repository around the listed path).
  const tip = picked !== null && !picked.git && listing !== null && listing.repo === null;
  const inside = picked !== null && !picked.git && listing !== null && listing.repo !== null;
  const trimmed = name.trim();
  const canAdd = picked !== null && trimmed !== "" && !busy && !down;

  async function submit() {
    if (!picked || !canAdd) return;
    const init = tip && gitInit;
    const root = picked.path;
    const id = commandId.idFor(`${root}\n${trimmed}\n${init}`);
    setBusy(true);
    setFormError(null);
    try {
      const answer = await callRef.current((t) =>
        t.projectAdd({ root, name: trimmed, ...(init ? { git_init: true } : {}), command_id: id }),
      );
      commandId.clear();
      let branch: string | null = null;
      if (init && listing) {
        // The folder is a repository now: its branch comes from one listing of its parent.
        try {
          const fresh = await callRef.current((t) => t.folderList({ path: listing.path }));
          branch = fresh.entries.find((e) => e.path === root)?.branch ?? null;
        } catch {
          branch = null;
        }
      }
      if (live.current) setAdded({ project: answer.project, root, initialised: init, branch });
      await onAdded();
    } catch (e) {
      const err = asTransportError(e);
      if (err.kind !== "unreachable") commandId.clear();
      if (live.current) setFormError(err.message);
    } finally {
      if (live.current) setBusy(false);
    }
  }

  function another() {
    const path = listing?.path;
    setAdded(null);
    void list(path);
  }

  const crumbs = listing ? crumbsOf(listing) : [];

  const body = added ? (
    <div className="ap-done">
      <span className="ap-done-tile">
        <Icon name="check" size={18} />
      </span>
      <h3 className="ap-done-title">{`${added.project.name} is a project`}</h3>
      <p className="ap-text">
        {`It shows in Projects and the sidebar. Sessions start in `}
        <span className="mono ap-inline">{added.root}</span>
        {` on ${serverName}.`}
      </p>
      {added.initialised ? (
        <p className="ap-git-line">
          <Icon name="git-branch" size={14} />
          <span>
            {added.branch
              ? `Ran git init in the folder. It is now a git repository on branch ${added.branch}.`
              : "Ran git init in the folder. It is now a git repository."}
          </span>
        </p>
      ) : null}
      <div className="ap-done-actions">
        <Button onClick={another}>Add another</Button>
        <Button variant="primary" onClick={() => onClose(added.project.id)}>
          Open project
        </Button>
      </div>
    </div>
  ) : (
    <>
      <div className="ap-body">
        <section className="ap-section" aria-label="Host">
          <span className="ap-label">Host</span>
          <div className="ap-host">
            <Icon name="server" size={16} />
            <span className="ap-host-name">{serverName}</span>
            <span className="ap-host-state">
              <span className={cx("ap-dot", down && "ap-dot-down")} aria-hidden="true" />
              {down ? "Not connected" : "Connected"}
            </span>
            <span className="ap-spacer" />
            <NotBuilt as="item" label="Other hosts" />
          </div>
        </section>

        <section className="ap-section" aria-label="How">
          <span className="ap-label">How</span>
          <div className="ap-how">
            <div className="ap-card ap-card-on">
              <div className="ap-card-top">
                <span className="ap-card-tile">
                  <Icon name="folder" size={14} />
                </span>
                <span className="ap-spacer" />
                <span className="ap-card-check">
                  <Icon name="check" size={16} />
                </span>
              </div>
              <span className="ap-card-title">Browse folder</span>
              <span className="ap-card-text">{`Pick a folder that is already on ${serverName}.`}</span>
            </div>
            <div className="ap-card ap-card-later">
              <NotBuilt as="button" label="Clone from URL" icon="copy" />
              <span className="ap-card-text">{`Clone a repository onto ${serverName}.`}</span>
            </div>
            <div className="ap-card ap-card-later">
              <NotBuilt as="button" label="Create new project" icon="plus" />
              <span className="ap-card-text">Start an empty repository.</span>
            </div>
          </div>
        </section>

        <section className="ap-section ap-folder" aria-label="Folder">
          <div className="ap-folder-head">
            <span className="ap-label">Folder</span>
            <span className="ap-spacer" />
            <span className="ap-note">Only folders inside your home folder</span>
          </div>
          <div className="ap-browser">
            <div className="ap-crumbs" role="group" aria-label="Folder path">
              <IconButton
                icon="arrow-left"
                title="Up one folder"
                disabled={listing === null || listing.parent === null}
                onClick={() => {
                  if (listing?.parent) void list(listing.parent);
                }}
              />
              {crumbs.map((crumb, i) => (
                <span className="ap-crumb" key={crumb.path}>
                  {i > 0 ? <span className="ap-crumb-sep">/</span> : null}
                  {i === crumbs.length - 1 ? (
                    <span className="ap-crumb-now" aria-current="location" title={crumb.home ? crumb.path : undefined}>
                      {crumb.home ? <Icon name="house" size={13} /> : null}
                      {crumb.label}
                    </span>
                  ) : (
                    <button
                      type="button"
                      className="ap-crumb-btn"
                      title={crumb.home ? crumb.path : undefined}
                      onClick={() => void list(crumb.path)}
                    >
                      {crumb.home ? <Icon name="house" size={13} /> : null}
                      {crumb.label}
                    </button>
                  )}
                </span>
              ))}
            </div>
            <ul className="ap-rows" aria-label="Folders">
              {listing === null && listError === null ? <li className="ap-empty">Loading folders…</li> : null}
              {listing !== null && listing.entries.length === 0 ? <li className="ap-empty">No folders here.</li> : null}
              {listing?.entries.map((entry) => {
                const taken = entry.project_id !== null;
                const selected = picked?.path === entry.path;
                return (
                  <li className={cx("ap-row", selected && "ap-row-selected")} key={entry.path}>
                    <button
                      type="button"
                      className="ap-row-main"
                      disabled={taken}
                      aria-pressed={selected}
                      onClick={() => pick(entry)}
                    >
                      <Icon name="folder" size={16} />
                      <span className="ap-row-name">{entry.name}</span>
                      {" "}
                      {entry.git ? (
                        <span className="ap-git">
                          <Icon name="git-branch" size={11} />
                          {entry.branch ? `git · ${entry.branch}` : "git"}
                        </span>
                      ) : null}
                      {" "}
                      {taken ? <span className="ap-taken">Already a project</span> : null}
                    </button>
                    {taken ? null : (
                      <IconButton
                        icon="chevron-right"
                        size={28}
                        title={`Open ${entry.name}`}
                        onClick={() => void list(entry.path)}
                      />
                    )}
                  </li>
                );
              })}
            </ul>
          </div>
          {listError ? (
            <p className="ap-error" role="alert">
              {listError}
            </p>
          ) : null}
          {listing?.truncated ? <p className="ap-note">Only the first 500 folders are shown.</p> : null}
        </section>

        <div className="ap-field">
          <label className="ap-label" htmlFor="ap-name">
            Project name
          </label>
          <Input
            id="ap-name"
            type="text"
            autoComplete="off"
            spellCheck={false}
            value={name}
            disabled={picked === null}
            onChange={(e) => setName(e.target.value)}
          />
          <span className="ap-path mono">{picked?.path ?? ""}</span>
        </div>

        {tip ? (
          <div className="ap-tip">
            <div className="ap-tip-head">
              <Icon name="sparkles" size={16} />
              <div className="ap-tip-text">
                <span className="ap-tip-title">Tip: make it a git repository</span>
                <span className="ap-tip-line">Pulse Factory can set it up when you add it.</span>
              </div>
            </div>
            <label className="ap-check">
              <input type="checkbox" checked={gitInit} onChange={(e) => setGitInit(e.target.checked)} />
              <span>
                Run <span className="mono">git init</span> in this folder when I add it
              </span>
            </label>
            <p className="ap-note ap-tip-hint">
              Or run <span className="mono">git init</span> {`on ${serverName} yourself.`}
            </p>
          </div>
        ) : null}
        {inside ? (
          <p className="ap-inside">
            {`This folder is inside the git repository ${listing?.repo ?? ""}. It is added as a folder.`}
          </p>
        ) : null}
        {formError ? (
          <p className="ap-error" role="alert">
            {formError}
          </p>
        ) : null}
      </div>
      <div className="ap-footer">
        <span className="ap-spacer" />
        <Button size="lg" onClick={() => onClose()}>
          Cancel
        </Button>
        <Button size="lg" variant="primary" icon="plus" disabled={!canAdd} onClick={() => void submit()}>
          Add project
        </Button>
      </div>
    </>
  );

  return createPortal(
    <div className="ap-overlay">
      <div className="add-project" role="dialog" aria-modal="true" aria-label="Add a project" ref={panel}>
        <div className="ap-head">
          <div className="ap-head-text">
            <h2 className="ap-title">Add a project</h2>
            <p className="ap-sub">A project is a folder on your server. Its sessions and runs start there.</p>
          </div>
          <IconButton icon="x" size={28} title="Close" onClick={() => onClose()} />
        </div>
        {body}
      </div>
    </div>,
    document.body,
  );
}
