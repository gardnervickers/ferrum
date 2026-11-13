#!/usr/bin/env python3
"""millgui's bridge to the job compiler and the conversational generators.

millgui runs this as a child process with the config folder as its working
directory and talks to it in JSON lines: one request object per line on
stdin, one reply per line on stdout. Nothing here touches LinuxCNC or HAL:
millgui sends the status fields the session needs, loads the programs this
returns, and copies the ``pins`` of every reply to its ``mymill-job`` HAL
component.

The rules (stamp, skip pins, stop after, refusing a stale build) all stay in
``mymill_vcp.job_session``, shared with the Probe Basic JOB page; the
G-code stays in ``mill_ops``.
"""

import importlib.util
import json
import os
import sys
import traceback
from types import SimpleNamespace

sys.path.insert(0, os.getcwd())

from mymill_job import JobError, conversational  # noqa: E402
from mymill_job.machine_files import read_tool_table  # noqa: E402
from mymill_job.compiler import Machine  # noqa: E402
from mymill_job.model import MAX_OPS  # noqa: E402
from mymill_vcp import job_session  # noqa: E402

# millgui sends interp_state as 1 (idle) or 2 (anything else).
job_session.INTERP_IDLE = 1

KIND_BADGES = {'conversational': 'CONV', 'cam': 'CAM', 'step': 'STEP'}
CLASSES = {'face': 'MillFaceOps', 'drill': 'MillDrillOps', 'helical_bore': 'MillHelicalBoreOps',
           'thread_mill': 'MillThreadMillOps'}


def _load_vendored(name):
    path = os.path.join(os.getcwd(), 'vendor', 'probe_basic', 'src', 'widgets', 'conversational', name + '.py')
    spec = importlib.util.spec_from_file_location('millgui_' + name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _fmt(value):
    text = '{:.3f}'.format(float(value)).rstrip('0').rstrip('.')
    return text or '0'


def _tool_comments(path):
    out = {}
    try:
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line.upper().startswith('T') or ';' not in line:
                    continue
                head, comment = line.split(';', 1)
                try:
                    out[int(head.split()[0][1:])] = comment.strip()
                except (ValueError, IndexError):
                    pass
    except OSError:
        pass
    return out


class Helper(object):
    def __init__(self):
        self.session = None
        self.cfg = {}
        self.mill_ops = conversational.load_mill_ops()
        self.threads = _load_vendored('thread_presets')
        self.message = ''

    # -- machine ---------------------------------------------------------------

    def machine(self):
        c = self.cfg
        return Machine(tools=read_tool_table(c['tool_table']), probe_tool=int(c.get('probe_tool') or 99),
                       max_forward_rpm=c.get('max_forward_rpm'), max_reverse_rpm=c.get('max_reverse_rpm'))

    # -- snapshot ----------------------------------------------------------------

    def _detail(self, op, comments):
        if op.kind == 'step':
            return 'PAUSES AT MACHINE TOP  ·  SPINDLE AND COOLANT OFF'
        if op.kind == 'conversational':
            p = op.params
            parts = ['T{}'.format(p.get('tool_number')), comments.get(p.get('tool_number'), ''),
                     '{} RPM'.format(_fmt(p.get('spindle_rpm', 0))), p.get('wcs', '')]
            return '  ·  '.join(str(x).upper() for x in parts if x)
        if op.kind == 'cam':
            setup = op.params.get('setup', {})
            return 'T{}  ·  FROM {}'.format(setup.get('tool'), str(op.params.get('source', '')).upper())
        return ''

    def snapshot(self):
        s = self.session
        run = s.run
        comments = _tool_comments(self.cfg['tool_table'])
        ops = []
        for position, op in enumerate(s.job.ops, start=1):
            bp = s.build_position(op.id)
            if run.running and run.position is not None and bp is not None:
                if bp < run.position:
                    state = 'done' if op.enabled else 'skipped'
                elif bp == run.position:
                    state = 'current' if op.enabled else 'skipped'
                else:
                    state = 'pending' if op.enabled else 'skipped'
            else:
                state = 'idle' if op.enabled else 'skipped'
            ops.append({'id': op.id, 'pos': position, 'name': op.name.upper(),
                        'badge': KIND_BADGES.get(op.kind, op.kind.upper()), 'kind': op.kind,
                        'enabled': bool(op.enabled), 'detail': self._detail(op, comments),
                        'state': state, 'can_toggle': s.can_toggle(op.id)})
        ready, why = s.readiness()
        if run.running:
            total = len(s.build.ranges) if s.build else 0
            text = 'RUNNING OP {} OF {}'.format(run.position, total) if run.position else 'RUNNING'
            tone = 'info'
        elif ready:
            text, tone = 'BUILT · LOADED', 'info'
        elif s.build is None:
            text, tone = 'NOT BUILT', ''
        else:
            text, tone = 'REBUILD NEEDED', 'warn'
        note, note_tone = '', ''
        warnings = s.warnings()
        if not ready and s.build is not None and not run.running:
            note, note_tone = why, 'warn'
        elif warnings:
            note, note_tone = warnings[0], 'warn'
        elif s.stop_requested:
            note = 'THE JOB WILL STOP WITH THE SPINDLE OFF BEFORE THE NEXT OP.'
        progress = ''
        if run.running and run.line and run.last_line:
            progress = 'LINE {} OF {}'.format(run.line - run.first_line + 1, run.last_line - run.first_line + 1)
        pins = s.pins
        return {
            'name': s.job.name.upper(), 'path': s.path or '', 'ops': ops,
            'state': text, 'tone': tone, 'ready': ready, 'note': note, 'note_tone': note_tone,
            'locked': s.locked, 'running': run.running, 'position': run.position or 0,
            'op_fraction': run.op_fraction, 'job_fraction': run.job_fraction, 'progress': progress,
            'stop_requested': s.stop_requested,
            'pins': {'stamp': int(pins.read('stamp')),
                     'skip': [p for p in range(1, MAX_OPS + 1) if pins.read('skip-{}'.format(p))],
                     'stop_after': bool(pins.read('stop-after'))},
        }

    # -- requests ----------------------------------------------------------------

    def handle(self, req):
        cmd = req.get('cmd')
        fn = getattr(self, 'cmd_' + str(cmd), None)
        if fn is None:
            raise ValueError('unknown command {!r}'.format(cmd))
        out = fn(req) or {}
        if self.session is not None and 'snap' not in out and cmd not in ('cycle_start', 'conv', 'thread', 'files', 'presets'):
            out['snap'] = self.snapshot()
        return out

    def cmd_init(self, req):
        self.cfg = req
        os.makedirs(req['folder'], exist_ok=True)
        self.session = job_session.JobSession(req['folder'], job_session.FakePins(), self.machine,
                                              loader=self._load)
        self.session.restore_last()
        return {'presets': [p.name for p in self.threads.PRESETS]}

    def _load(self, path):
        self.loaded = path

    def _stat(self, req):
        st = req.get('stat') or {}
        return SimpleNamespace(file=st.get('file', ''), interp_state=1 if st.get('idle', True) else 2,
                               motion_line=st.get('motion_line', 0), current_line=st.get('current_line', 0),
                               paused=st.get('paused', False))

    def cmd_poll(self, req):
        self.session.poll(self._stat(req))
        if req.get('publish'):
            self.session.publish()

    def cmd_cycle_start(self, req):
        return {'problem': self.session.cycle_start_problem(self._stat(req), int(req.get('line') or 0))}

    def cmd_files(self, req):
        paths = self.session.job_files()
        names = [os.path.splitext(os.path.basename(p))[0].replace('_', ' ').upper() for p in paths]
        return {'paths': paths, 'names': names}

    def cmd_new(self, req):
        self.session.new()

    def cmd_open(self, req):
        self.session.open(req['path'])

    def cmd_add_step(self, req):
        message = req.get('message') or job_session.STEP_PRESETS[int(req.get('index', 0))]
        pos = self.session.add_step(message)
        return {'message': 'ADDED STEP AS OP {}.'.format(pos)}

    def cmd_remove(self, req):
        self.session.remove(req['id'])

    def cmd_move(self, req):
        self.session.move(req['id'], int(req['offset']))

    def cmd_set_enabled(self, req):
        self.session.set_enabled(req['id'], bool(req['enabled']))

    def cmd_start_at(self, req):
        self.session.start_at(req['id'])

    def cmd_stop_after(self, req):
        self.session.request_stop_after(bool(req.get('on')))

    def cmd_build(self, req):
        self.loaded = None
        compiled = self.session.build_and_load()
        out = {'load': self.loaded or self.session.build_path}
        if compiled.warnings:
            out['message'], out['tone'] = compiled.warnings[0], 'warn'
        else:
            out['message'], out['tone'] = 'BUILT {} OPS AND LOADED THEM.'.format(len(compiled.ranges)), 'ready'
        return out

    # -- conversational ------------------------------------------------------------

    def cmd_presets(self, req):
        return {'presets': [p.name for p in self.threads.PRESETS]}

    def cmd_thread(self, req):
        """A preset's major diameter, pitch and tap drill in the given units."""
        preset = self.threads.find_preset(req['name'])
        if preset is None:
            return {'found': False}
        major, pitch, drill = self.threads.preset_values(preset, req.get('units', 'MM'))
        return {'found': True, 'major': major, 'pitch': pitch, 'drill': drill}

    def _generator(self, req):
        op_type = req['type']
        gen = getattr(self.mill_ops, CLASSES[op_type])()
        for key, value in (req.get('attrs') or {}).items():
            if key == 'holes':
                gen.holes = [tuple(h) for h in value]
            elif key == 'center':
                gen.center = tuple(value)
            elif key in ('tool_number', 'radial_passes'):
                setattr(gen, key, int(value))
            else:
                setattr(gen, key, value)
        if op_type in ('helical_bore', 'thread_mill'):
            gen.z_feed = gen.xy_feed
        machine = self.machine()
        tool = machine.tool(gen.tool_number)
        if hasattr(gen, 'tool_diameter'):
            gen.tool_diameter = tool.diameter if tool else 0.
        gen.max_spindle_rpm = machine.max_rpm(gen.spindle_dir.strip().upper())
        return gen

    def _lines(self, req, gen):
        if req['type'] == 'face':
            return gen.face()
        if req['type'] == 'drill':
            return gen.generate(req.get('cycle', 'DRILL'), float(req.get('cycle_param') or 0.))
        return gen.generate()

    def cmd_conv(self, req):
        gen = self._generator(req)
        cycle = req.get('cycle', 'DRILL')
        try:
            lines = self._lines(req, gen)
        except self.mill_ops.OperationError as exc:
            return {'errors': exc.errors}
        summary = gen.summary(cycle) if req['type'] == 'drill' else gen.summary()
        action = req.get('action', 'check')
        out = {'errors': [], 'summary': summary}
        if action == 'add':
            pos = self.session.add_from_generator(gen, cycle if req['type'] == 'drill' else None,
                                                  req.get('cycle_param') if req['type'] == 'drill' else None)
            out['message'] = 'ADDED TO {} AS OP {}. HOLD BUILD AND LOAD ON THE JOB PAGE.'.format(
                self.session.job.name.upper(), pos)
            out['snap'] = self.snapshot()
        elif action == 'post':
            path = self._free_program_path(req.get('name') or 'conv')
            with open(path, 'w') as f:
                f.write(self.mill_ops.program_text([lines]))
            out['load'] = path
            out['message'] = '{} SAVED TO {}.'.format(summary, os.path.basename(path))
        return out

    def _free_program_path(self, name):
        folder = self.cfg['nc_folder']
        os.makedirs(folder, exist_ok=True)
        name = ''.join(c if (c.isalnum() or c in ' ._-') else '_' for c in os.path.basename(name)).strip(' .')
        name = (name or 'conv').replace(' ', '_')
        if name.lower().endswith('.ngc'):
            name = name[:-4]
        base = os.path.join(folder, name)
        path, i = base + '.ngc', 1
        while os.path.exists(path):
            path = '{}_{:d}.ngc'.format(base, i)
            i += 1
        return path


def main():
    helper = Helper()
    out = sys.stdout
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            reply = helper.handle(json.loads(line))
            reply['ok'] = True
        except JobError as exc:
            errors = list(exc.errors)
            reply = {'ok': False, 'error': errors[0] if len(errors) == 1 else
                     '{} (+{} more)'.format(errors[0], len(errors) - 1), 'errors': errors}
        except Exception as exc:  # report, keep serving
            traceback.print_exc(file=sys.stderr)
            reply = {'ok': False, 'error': '{}: {}'.format(type(exc).__name__, exc)}
        if helper.session is not None and 'snap' not in reply and reply.get('ok') is False:
            try:
                reply['snap'] = helper.snapshot()
            except Exception:
                pass
        out.write(json.dumps(reply) + '\n')
        out.flush()


if __name__ == '__main__':
    main()
