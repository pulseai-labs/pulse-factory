// The Projects page (2c): the live projects from `projectList()` and the "Add project" form.
import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { newCommandId } from "../components/command-id";
import { clock } from "../components/format";
import { asTransportError, isDownKind, useConnection } from "../state/connection";
import { Button, Input } from "../ui";

export function Projects() {
  const { call, setProjects, epoch, projects, projectsAt, link, server } = useConnection();
  const [pageError, setPageError] = useState<string | null>(null);
  const [root, setRoot] = useState("");
  const [name, setName] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The command ID of an add that ended with `unreachable`: a retry of the same submit reuses it,
  // so the server admits the add once (A7).
  const pending = useRef<{ root: string; name: string; id: string } | null>(null);
  const live = useRef(true);

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    try {
      const list = await call((t) => t.projectList());
      setProjects(list.projects);
      if (live.current) setPageError(null);
    } catch (e) {
      const err = asTransportError(e);
      // A disconnect shows the banner. Any other refusal is one error line on this page.
      if (live.current) setPageError(isDownKind(err.kind) ? null : err.message);
    }
  }, [call, setProjects]);

  // On opening the page, and once when the link comes back (`epoch`). Never on a timer.
  useEffect(() => {
    void load();
  }, [load, epoch]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    const r = root.trim();
    const n = name.trim();
    if (r === "") {
      setFormError("Enter the absolute path of the project's folder on the server.");
      return;
    }
    setFormError(null);
    setBusy(true);
    const same = pending.current && pending.current.root === r && pending.current.name === n;
    const id = same && pending.current ? pending.current.id : newCommandId();
    pending.current = { root: r, name: n, id };
    try {
      await call((t) => t.projectAdd({ root: r, ...(n ? { name: n } : {}), command_id: id }));
      pending.current = null;
      if (live.current) {
        setRoot("");
        setName("");
      }
      await load();
    } catch (err) {
      const te = asTransportError(err);
      if (te.kind !== "unreachable") pending.current = null;
      if (live.current) setFormError(te.message);
    } finally {
      if (live.current) setBusy(false);
    }
  }

  const count = projects?.length ?? 0;
  return (
    <section className="page" aria-labelledby="projects-title">
      <div className="page-head">
        <h2 id="projects-title">Projects</h2>
        <p className="muted">
          {projects === null
            ? `Projects on ${server.address}`
            : `${count} ${count === 1 ? "project" : "projects"} on ${server.address}`}
        </p>
        {link.state === "down" && projectsAt !== null ? (
          <p className="stale">{`As of ${clock(projectsAt)}`}</p>
        ) : null}
      </div>

      {pageError ? (
        <p className="page-error" role="alert">
          <span>{pageError}</span>
          <Button onClick={() => void load()}>
            Retry now
          </Button>
        </p>
      ) : null}

      {projects === null ? (
        <p className="muted">Loading…</p>
      ) : projects.length === 0 ? (
        <p className="empty">No projects yet. Add one below.</p>
      ) : (
        <ul className="project-list">
          {projects.map((p) => (
            <li key={p.id}>
              <span className="project-name">{p.name}</span>
              <span className="project-root mono">{p.root}</span>
            </li>
          ))}
        </ul>
      )}

      <form className="add-form" onSubmit={submit} noValidate aria-label="Add project">
        <h3>Add project</h3>
        <label htmlFor="pf-root">Project root</label>
        <Input
          mono
          id="pf-root"
          type="text"
          autoComplete="off"
          spellCheck={false}
          placeholder="/home/you/code/project"
          value={root}
          disabled={busy}
          onChange={(e) => setRoot(e.target.value)}
        />
        <label htmlFor="pf-project-name">Name (optional)</label>
        <Input
          id="pf-project-name"
          type="text"
          autoComplete="off"
          value={name}
          disabled={busy}
          onChange={(e) => setName(e.target.value)}
        />
        {formError ? (
          <p className="problem-title" role="alert">
            {formError}
          </p>
        ) : null}
        <Button type="submit" disabled={busy}>
          Add project
        </Button>
      </form>
    </section>
  );
}
