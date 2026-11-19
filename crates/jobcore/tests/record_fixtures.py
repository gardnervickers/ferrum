#!/usr/bin/env python3
"""Record the Python job helper's replies for the jobcore conformance test.

Development tool only; ferrum never runs it. From the repository root:

    python3 ferrum/crates/jobcore/tests/record_fixtures.py

For every ``tests/fixtures/<name>.jsonl`` this runs ``tests/job_helper.py``'s (the original Python helper, kept only as the reference)
``main()`` in a fresh temporary config folder (holding a copy of
``tests/fixtures/tool.tbl``) and writes ``<name>.expected.jsonl``: one line
per request with the reply and a digest of every file in the folder after it,
then a last line with the final file contents. ``tests/conformance.rs`` replays
the same requests through the Rust ``Helper`` and compares.

Op ids come from a counter and new jobs are named ``JOB 01-02 0304`` so the
output is deterministic; the Rust test uses the same hooks.

Fixture lines are requests, or harness actions (``_write``, ``_replace``,
``_copy``, ``_remove``). Strings may use ``$TMP`` (the config folder),
``$LOAD`` (the last reply's ``load``), ``$OP<n>`` (the id of op n in the last
snapshot), ``$OPLINE<n>[+-k]`` and ``$STOPLINE<n>`` (line numbers in the
``$LOAD`` program).
"""

import hashlib
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import types

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, 'fixtures')
REPO = os.path.abspath(os.path.join(HERE, '..', '..', '..', '..'))


def tree(root):
    files = {}
    for folder, _, names in os.walk(root):
        for name in names:
            path = os.path.join(folder, name)
            with open(path, 'rb') as f:
                files[os.path.relpath(path, root)] = f.read()
    return files


def digest(files, tmp):
    h = hashlib.sha256()
    for path in sorted(files):
        h.update(path.encode() + b'\0' + files[path].replace(tmp.encode(), b'$TMP') + b'\0')
    return h.hexdigest()


class Context(object):
    def __init__(self, tmp):
        self.tmp = tmp
        self.load = ''
        self.ops = []

    def program_lines(self):
        with open(os.path.join(self.tmp, self.load)) as f:
            return f.read().split('\n')

    def sub(self, value):
        if isinstance(value, dict):
            return {k: self.sub(v) for k, v in value.items()}
        if isinstance(value, list):
            return [self.sub(v) for v in value]
        if not isinstance(value, str):
            return value
        m = re.match(r'^\$OPLINE(\d+)([+-]\d+)?$', value)
        if m:
            lines = self.program_lines()
            first = next(i for i, l in enumerate(lines, 1) if l.startswith('(MYMILL-OP {}/'.format(m.group(1))))
            return first + int(m.group(2) or 0)
        m = re.match(r'^\$STOPLINE(\d+)$', value)
        if m:
            lines = self.program_lines()
            return next(i for i, l in enumerate(lines, 1) if l == 'o<job_op{}_stop> endif'.format(m.group(1))) - 1
        m = re.match(r'^\$OP(\d+)$', value)
        if m:
            return self.ops[int(m.group(1)) - 1]
        return value.replace('$TMP', self.tmp).replace('$LOAD', self.load)

    def note(self, reply):
        if reply.get('load'):
            self.load = reply['load']
        snap = reply.get('snap')
        if isinstance(snap, dict):
            self.ops = [op['id'] for op in snap['ops']]

    def action(self, entry):
        path = lambda p: os.path.join(self.tmp, self.sub(p))
        if '_write' in entry:
            with open(path(entry['_write']), 'w') as f:
                f.write(self.sub(entry['text']))
        elif '_replace' in entry:
            with open(path(entry['_replace'])) as f:
                text = f.read()
            with open(path(entry['_replace']), 'w') as f:
                f.write(text.replace(self.sub(entry['old']), self.sub(entry['new'])))
        elif '_copy' in entry:
            shutil.copyfile(path(entry['_copy']), path(entry['to']))
        elif '_remove' in entry:
            os.remove(path(entry['_remove']))
        else:
            raise ValueError(entry)


def record_one(name):
    tmp = os.path.realpath(tempfile.mkdtemp(prefix='jobcore-'))
    shutil.copyfile(os.path.join(FIXTURES, 'tool.tbl'), os.path.join(tmp, 'tool.tbl'))
    os.chdir(tmp)
    sys.path.insert(0, REPO)

    import mymill_job.model as model
    counter = [0]

    def token_hex(n):
        counter[0] += 1
        return '{:08x}'.format(counter[0])

    model.secrets = types.SimpleNamespace(token_hex=token_hex)
    from mymill_vcp import job_session
    job_session._default_job_name = lambda: 'JOB 01-02 0304'

    spec = importlib.util.spec_from_file_location('job_helper', os.path.join(REPO, 'ferrum', 'crates', 'jobcore', 'tests', 'job_helper.py'))
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)

    def load_vendored(module_name):
        path = os.path.join(REPO, 'vendor', 'probe_basic', 'src', 'widgets', 'conversational', module_name + '.py')
        s = importlib.util.spec_from_file_location('ferrum_' + module_name, path)
        m = importlib.util.module_from_spec(s)
        s.loader.exec_module(m)
        return m

    helper._load_vendored = load_vendored

    with open(os.path.join(FIXTURES, name + '.jsonl')) as f:
        entries = [json.loads(line) for line in f if line.strip() and not line.startswith('//')]

    ctx = Context(tmp)
    replies = []
    records = []

    class Out(object):
        def write(self, text):
            replies.append(text)

        def flush(self):
            pass

    def lines():
        for entry in entries:
            if isinstance(entry, dict) and any(k.startswith('_') for k in entry):
                ctx.action(entry)
                continue
            count = len(replies)
            yield json.dumps(ctx.sub(entry)) + '\n'
            assert len(replies) == count + 1
            text = replies[-1].rstrip('\n')
            ctx.note(json.loads(text))
            if os.environ.get('RECORD_DEBUG'):
                sys.stderr.write('>> {}\n<< {}\n'.format(json.dumps(entry), text[:400]))
            records.append({'reply': text.replace(tmp, '$TMP'), 'tree': digest(tree(tmp), tmp)})

    old_in, old_out = sys.stdin, sys.stdout
    sys.stdin, sys.stdout = lines(), Out()
    try:
        helper.main()
    finally:
        sys.stdin, sys.stdout = old_in, old_out
    files = {p: c.decode('utf-8') for p, c in sorted(tree(tmp).items())}
    with open(os.path.join(FIXTURES, name + '.expected.jsonl'), 'w') as f:
        for r in records:
            f.write(json.dumps(r) + '\n')
        f.write(json.dumps({'files': files}, indent=None, sort_keys=True).replace(tmp, '$TMP') + '\n')
    shutil.rmtree(tmp)
    print('{}: {} requests'.format(name, len(records)))


def main():
    if len(sys.argv) > 1:
        record_one(sys.argv[1])
        return
    for fixture in sorted(os.listdir(FIXTURES)):
        if fixture.endswith('.jsonl') and not fixture.endswith('.expected.jsonl'):
            subprocess.check_call([sys.executable, os.path.abspath(__file__), fixture[:-len('.jsonl')]])


if __name__ == '__main__':
    main()
