import importlib.util
import json
import pathlib
import subprocess
import sys
import threading
import time
import os
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('runner', pathlib.Path(__file__).with_name('runner.py'))
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)

class ResearchTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)
        self.repo = self.root / 'project'
        self.repo.mkdir()
        subprocess.run(['git', 'init', '-q', str(self.repo)], check=True)
        self.git('config', 'user.email', 'test@example.invalid')
        self.git('config', 'user.name', 'Test')
        (self.repo / 'AGENTS.md').write_text('Read the requirements.\n')
        (self.repo / 'app.txt').write_text('committed\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'fixture')
    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.repo), *args], text=True).strip()
    def test_snapshot_uses_selected_commit_and_leaves_dirty_work_alone(self):
        (self.repo / 'app.txt').write_text('unfinished user work\n')
        target = self.root / 'snapshot'
        sha, omitted = r.snapshot(self.repo, 'HEAD', target)
        self.assertEqual(sha, self.git('rev-parse', 'HEAD'))
        self.assertEqual((target / 'app.txt').read_text(), 'committed\n')
        self.assertEqual((self.repo / 'app.txt').read_text(), 'unfinished user work\n')
    def test_snapshot_does_not_follow_symlinks(self):
        (self.repo / 'outside').symlink_to('/etc/passwd')
        self.git('add', 'outside'); self.git('commit', '-qm', 'symlink')
        _, omitted = r.snapshot(self.repo, 'HEAD', self.root / 'snapshot')
        self.assertFalse((self.root / 'snapshot/outside').exists())
        self.assertIn('outside', omitted)
    def test_apply_rejects_changed_resource_before_writing_anything(self):
        before = {'AGENTS.md': 'Read the requirements.\n'}
        after = {'AGENTS.md': 'Check layouts too.\n', '.agents/skills/layout/SKILL.md': 'New skill\n'}
        proposal = r.proposal(before, after)
        (self.repo / 'AGENTS.md').write_text('new user edit\n')
        with self.assertRaises(ValueError):
            r.apply_proposal(self.repo, proposal)
        self.assertFalse((self.repo / '.agents').exists())
        self.assertEqual((self.repo / 'AGENTS.md').read_text(), 'new user edit\n')
    def test_apply_only_changes_proposed_resources(self):
        proposal = r.proposal({'AGENTS.md': 'Read the requirements.\n'}, {'AGENTS.md': 'Check layouts too.\n'})
        r.apply_proposal(self.repo, proposal)
        self.assertEqual((self.repo / 'AGENTS.md').read_text(), 'Check layouts too.\n')
        self.assertEqual((self.repo / 'app.txt').read_text(), 'committed\n')
    def test_rejects_paths_outside_resource_surface_and_symlink_parents(self):
        for path in ('../outside', '/tmp/outside', '.git/config', 'src/main.rs', '.codex/config.toml'):
            with self.assertRaises(ValueError):
                r.proposal({}, {path: 'bad'})
        (self.repo / '.agents').symlink_to(self.root / 'elsewhere')
        with self.assertRaises(ValueError):
            r.apply_proposal(self.repo, r.proposal({}, {'.agents/skills/a/SKILL.md': 'x'}))
    def test_codex_uses_bounded_permissions_and_never_bypass_flags(self):
        args = r.codex_argv(self.root, self.root / 'last.txt')
        self.assertIn('workspace-write', args)
        self.assertNotIn('--dangerously-bypass-approvals-and-sandbox', args)
        self.assertIn('--ignore-user-config', args)
    def test_real_harness_schedule_exports_review_without_touching_project(self):
        class FakeRuntime(r.Runtime):
            def invoke(self, prompt, workspace, label, stage):
                with self.serial:
                    self.reserve(); self.event(label, stage, 'done', 'Test double; no model call')
                    trace = self.root / f'test-{self.state["calls"]}.jsonl'
                    trace.write_text('{"type":"test_fixture"}\n')
                    harness = workspace / 'context/harness/AGENTS.md'
                    if harness.exists():
                        harness.write_text(harness.read_text() + '\nCheck narrow layouts and record the result.\n')
                    return '<edits>Added a layout check.</edits><experience>Verify narrow layouts.</experience>', trace
        support = self.repo / '.agents/skills/layout/references.md'
        support.parent.mkdir(parents=True); support.write_text('Keep this supporting document.\n')
        self.git('add', '.'); self.git('commit', '-qm', 'skill support')
        run = self.root / 'experiment'; run.mkdir()
        request = {'project': str(self.repo), 'revision': 'HEAD', 'problem': 'Improve evaluation of UI layout',
                   'tasks': ['Change the greeting and verify it.'], 'minutes': 10}
        r.write_json(run / 'state.json', {'calls': 0, 'max_calls': 10, 'events': []})
        native_run = r.subprocess.run
        def command(args, **kwargs):
            if args == ['codex', '--version']:
                return subprocess.CompletedProcess(args, 0, stdout='test double', stderr='')
            return native_run(args, **kwargs)
        with patch.object(r.subprocess, 'run', side_effect=command):
            r.execute(run, request, runtime_factory=FakeRuntime)
        state = json.loads((run / 'state.json').read_text())
        self.assertEqual(state['status'], 'review')
        self.assertEqual(state['calls'], 8)
        self.assertIsNone(state['cost_usd'])
        self.assertTrue((run / 'REPORT.md').is_file())
        changes = json.loads((run / 'proposal.json').read_text())
        self.assertIn('AGENTS.md', [c['path'] for c in changes])
        self.assertNotIn('.agents/skills/layout/references.md', [c['path'] for c in changes])
        for work in (run / 'executions').rglob('workspace'):
            self.assertEqual((work / '.agents/skills/layout/references.md').read_text(), 'Keep this supporting document.\n')
        self.assertEqual((self.repo / 'AGENTS.md').read_text(), 'Read the requirements.\n')
        self.assertEqual(len(list((run / 'executions').rglob('answer.md'))), 4)

    def test_cancel_terminates_the_active_model_process(self):
        run = self.root / 'cancel-run'; run.mkdir()
        work = run / 'work'; work.mkdir()
        runtime = r.Runtime(run, {'calls': 0, 'max_calls': 1, 'events': []}, minutes=1)
        pid_file = work / 'pid'
        script = "import os,pathlib,time; pathlib.Path('pid').write_text(str(os.getpid())); time.sleep(20)"
        def cancel():
            until = time.monotonic() + 3
            while not pid_file.exists() and time.monotonic() < until: time.sleep(.02)
            (run / 'cancel').touch()
        thread = threading.Thread(target=cancel); thread.start()
        with patch.object(r, 'codex_argv', return_value=[sys.executable, '-c', script]):
            with self.assertRaises(r.Cancelled): runtime.invoke('test', work, 'cancel-test', 'baseline')
        thread.join()
        self.assertTrue(pid_file.exists())
        with self.assertRaises(ProcessLookupError): os.kill(int(pid_file.read_text()), 0)
        self.assertEqual(runtime.state['calls'], 1)

    def test_cancel_and_call_budget_stop_before_launch(self):
        run = self.root / 'run'; run.mkdir()
        state = {'calls': 0, 'max_calls': 1, 'input_tokens': 0, 'output_tokens': 0}
        runtime = r.Runtime(run, state, minutes=1)
        (run / 'cancel').touch()
        with self.assertRaises(r.Cancelled): runtime.check()
        (run / 'cancel').unlink()
        state['calls'] = 1
        with self.assertRaises(RuntimeError): runtime.reserve()

if __name__ == '__main__': unittest.main()
