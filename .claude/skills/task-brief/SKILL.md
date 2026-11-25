---
name: task-brief
description: Template for briefing a subagent or new session in this repo so it has complete context in few tokens. Use before delegating or handing off work.
---
A fresh agent starts without this conversation. Fill every line, in under 250
words; write "none" rather than drop a line. Point to code as `path` and
symbol instead of pasting it. Do not repeat `CLAUDE.md`; name it to read.

    Goal: <one sentence: the observable result>
    Why: <the symptom or decision behind it>
    Read first: <crate CLAUDE.md; path: symbol, symbol>
    Known: <facts established, with evidence; mark guesses as guesses>
    Tried / ruled out: <what failed and why>
    Base: <Causeway branch and commit; related PRs, if any>
    Scope: <files it may change; files it must not>
    Constraints: <owner decisions, ordering, compatibility; always: must hold
      on other GPUs/vendors/drivers (query capabilities, test wave32 and
      wave64 or both paths, report adapters verified), see CLAUDE.md>
    Done when: <commands and expected result; tests to add>
    Budget: <when to stop and report what is left>
    Return: <format and length; include what was not verified>

Before sending, check that a reader with only this brief and the repository
could start without asking a question. If not, add the missing fact.
