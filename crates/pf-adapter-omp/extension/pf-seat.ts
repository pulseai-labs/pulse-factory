// Pulse Factory's own OMP extension (ADR-0011). OMP loads it with `--hook`.
//
// It uses only OMP's public extension API. It does four things:
//   - it appends one JSON event per line to the file in PF_EVENT_FILE;
//   - it delivers a message from the file in PF_INBOX_FILE, once the session is idle;
//   - it denies a tool call when the rule hook says deny, or when it cannot get an answer;
//   - it reports the token usage of each assistant message.
//
// Turn end is `agent_end` without `willContinue === true`. The extension reads no screen.
// Plain JavaScript: no type import, so it needs no package of OMP's.

export default function (pi) {
  const fs = require('node:fs')
  const cp = require('node:child_process')

  const EVENT_FILE = process.env.PF_EVENT_FILE
  const INBOX_FILE = process.env.PF_INBOX_FILE
  const HOOK_CMD = process.env.PF_HOOK_CMD
  const ROLE = process.env.PF_SEAT_ROLE || ''
  const HOOK_TIMEOUT_MS = 10000
  // The tools the seat may use. OMP loads MCP servers from project, user and plugin config
  // whatever `--tools` says, so the extension limits the tools itself.
  const ALLOWED = process.env.PF_ALLOWED_TOOLS ? new Set(process.env.PF_ALLOWED_TOOLS.split(',')) : null

  let lastCtx = null
  const commands = new Map() // tool call id -> bash command
  const pending = [] // messages waiting for an idle session
  let inboxOffset = 0
  let inboxBuffer = ''

  function emit(event) {
    if (!EVENT_FILE) return
    try {
      fs.appendFileSync(EVENT_FILE, JSON.stringify({ t: Date.now(), ...event }) + '\n')
    } catch {
      // A broken event file must not break the seat. The adapter sees the missing events.
    }
  }

  // The rule hook gets the call as JSON on stdin and answers {"decision":"allow"|"deny","reason":"..."}.
  // A missing hook, a failure and an answer it cannot read all deny: the extension fails closed.
  function decide(toolName, input) {
    if (!HOOK_CMD) return { block: true, reason: 'Pulse Factory: no rule hook is configured, so the call is denied' }
    try {
      const r = cp.spawnSync('/bin/sh', ['-c', HOOK_CMD], {
        input: JSON.stringify({ role: ROLE, tool: toolName, input }),
        encoding: 'utf8',
        timeout: HOOK_TIMEOUT_MS,
        maxBuffer: 1 << 20,
      })
      if (r.error || r.status !== 0) {
        return { block: true, reason: 'Pulse Factory: the rule hook failed, so the call is denied' }
      }
      const answer = JSON.parse(r.stdout)
      if (answer && answer.decision === 'allow') return null
      const why = answer && typeof answer.reason === 'string' && answer.reason ? answer.reason : 'denied by rule'
      return { block: true, reason: 'Pulse Factory rule: ' + why }
    } catch {
      return { block: true, reason: 'Pulse Factory: the rule hook gave no readable answer, so the call is denied' }
    }
  }

  function readInbox() {
    if (!INBOX_FILE) return
    try {
      const stat = fs.statSync(INBOX_FILE)
      if (stat.size <= inboxOffset) return
      const fd = fs.openSync(INBOX_FILE, 'r')
      try {
        const buf = Buffer.alloc(stat.size - inboxOffset)
        fs.readSync(fd, buf, 0, buf.length, inboxOffset)
        inboxOffset = stat.size
        inboxBuffer += buf.toString('utf8')
      } finally {
        fs.closeSync(fd)
      }
      let nl
      while ((nl = inboxBuffer.indexOf('\n')) >= 0) {
        const line = inboxBuffer.slice(0, nl)
        inboxBuffer = inboxBuffer.slice(nl + 1)
        if (!line.trim()) continue
        try {
          const m = JSON.parse(line)
          if (m && typeof m.text === 'string') pending.push(m.text)
        } catch {
          // A line it cannot read is skipped.
        }
      }
    } catch {
      // No inbox yet.
    }
  }

  function deliverIfIdle() {
    readInbox()
    if (pending.length === 0 || !lastCtx) return
    let idle = false
    try {
      idle = lastCtx.isIdle() === true
    } catch {
      idle = false
    }
    if (!idle) return
    const text = pending.shift()
    emit({ ev: 'delivered' })
    pi.sendUserMessage(text)
  }

  // Probe mode (PF_PROBE_SHAPE=1): write the shape of each OMP event, to qualify an OMP version.
  // It writes no event text that matters to the adapter, and a seat never runs with it.
  function shape(value, depth) {
    if (value === null || typeof value !== 'object') {
      return typeof value === 'string' ? value.slice(0, 300) : value
    }
    if (depth <= 0) return Array.isArray(value) ? '[array ' + value.length + ']' : '{' + Object.keys(value).join(',') + '}'
    if (Array.isArray(value)) return value.slice(0, 8).map((v) => shape(v, depth - 1))
    const out = {}
    for (const k of Object.keys(value).slice(0, 40)) {
      try {
        out[k] = typeof value[k] === 'function' ? 'function' : shape(value[k], depth - 1)
      } catch {
        out[k] = 'unreadable'
      }
    }
    return out
  }
  if (process.env.PF_PROBE_SHAPE === '1') {
    for (const name of ['session_start', 'before_agent_start', 'agent_start', 'turn_start', 'turn_end', 'message_start', 'message_end', 'tool_call', 'tool_result', 'agent_end', 'input', 'context', 'session_shutdown']) {
      pi.on(name, (event, ctx) => {
        let ctxKeys = []
        try { ctxKeys = Object.keys(ctx || {}) } catch {}
        emit({ ev: 'raw', name, event: shape(event, 3), ctxKeys })
      })
    }
    // The system prompt lists the skills, and the tool lists show which tools the seat has.
    pi.on('before_agent_start', (event) => {
      try {
        const prompt = Array.isArray(event && event.systemPrompt) ? event.systemPrompt.join('\n----\n') : String((event && event.systemPrompt) || '')
        if (process.env.PF_SESSION_DIR) fs.writeFileSync(process.env.PF_SESSION_DIR + '/probe-system-prompt.txt', prompt)
        const names = (list) => (Array.isArray(list) ? list.map((t) => (t && t.name) || t) : null)
        emit({ ev: 'inventory_all', allTools: names(pi.getAllTools && pi.getAllTools()) })
      } catch (e) {
        emit({ ev: 'inventory_all', error: String(e) })
      }
    })
    emit({ ev: 'raw', name: 'pi', piKeys: Object.keys(pi).concat(Object.getOwnPropertyNames(Object.getPrototypeOf(pi) || {})) })
  }

  const timer = setInterval(deliverIfIdle, 200)
  if (typeof timer.unref === 'function') timer.unref()

  pi.on('session_start', (_event, ctx) => {
    lastCtx = ctx
    emit({ ev: 'session_start' })
  })
  pi.on('agent_start', (_event, ctx) => {
    lastCtx = ctx
    emit({ ev: 'agent_start' })
  })
  pi.on('agent_end', (event, ctx) => {
    lastCtx = ctx
    emit({ ev: 'agent_end', willContinue: event && event.willContinue === true })
  })
  pi.on('message_end', (event, ctx) => {
    lastCtx = ctx
    const m = event && event.message
    if (m && m.role === 'assistant') emit({ ev: 'message_end', role: 'assistant', usage: m.usage || {} })
  })
  // Limit the tool list, and report the tools the seat has. The adapter ends a launch whose seat
  // holds a tool outside its list.
  pi.on('before_agent_start', (event) => {
    try {
      const names = (list) => (Array.isArray(list) ? list.map((t) => (t && t.name) || t) : null)
      if (ALLOWED && typeof pi.setActiveTools === 'function') pi.setActiveTools([...ALLOWED])
      // The system prompt lists the skills the seat holds: `<skills>` with one `- name: text` line each.
      const sp = event && event.systemPrompt
      const prompt = Array.isArray(sp) ? sp.join('\n') : String(sp || '')
      const block = /<skills>([\s\S]*?)<\/skills>/.exec(prompt)
      const skills = block ? [...block[1].matchAll(/^- ([^:\n]+):/gm)].map((m) => m[1].trim()) : []
      emit({ ev: 'inventory', activeTools: names(pi.getActiveTools && pi.getActiveTools()), skills })
    } catch (e) {
      emit({ ev: 'inventory', error: String(e) })
    }
  })
  pi.on('tool_call', (event, ctx) => {
    lastCtx = ctx
    const name = event && event.toolName
    const input = (event && event.input) || {}
    if (ALLOWED && name && !ALLOWED.has(name)) {
      const reason = 'Pulse Factory: the tool ' + name + ' is not on the seat tool list'
      emit({ ev: 'deny', tool: name, reason })
      return { block: true, reason }
    }
    if (name === 'bash' && event.toolCallId) commands.set(event.toolCallId, String(input.command || ''))
    emit({ ev: 'tool_call', tool: name })
    const verdict = decide(name, input)
    if (verdict) {
      emit({ ev: 'deny', tool: name, reason: verdict.reason })
      return verdict
    }
    return undefined
  })
  pi.on('tool_result', (event, ctx) => {
    lastCtx = ctx
    const name = event && event.toolName
    if (name !== 'bash') {
      emit({ ev: 'tool_result', tool: name })
      return
    }
    const id = event.toolCallId
    const command = (id && commands.get(id)) || String((event.input && event.input.command) || '')
    if (id) commands.delete(id)
    const details = event.details || {}
    let exitCode = typeof details.exitCode === 'number' ? details.exitCode : typeof event.exitCode === 'number' ? event.exitCode : event.isError ? 1 : 0
    // The output is not sent. A first push prints no SHA, an update prints a short one, and a seat
    // can print any text, so the adapter measures a push in git and never reads it from output.
    emit({ ev: 'bash', command, exitCode })
  })
}
