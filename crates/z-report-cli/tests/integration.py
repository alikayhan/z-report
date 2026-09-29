"""Exercise the packaged protocol and real worker processes with local fixtures."""
import datetime
import json
import os
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ENGINE = Path(os.environ.get('Z_REPORT_TEST_ENGINE', str(Path(__file__).resolve().parents[3] / 'target/debug/z-report-engine')))
FAKE = r'''
import json, os, subprocess, sys, time
from pathlib import Path
root=Path(os.environ['FIXTURE_ROOT'])
config=json.loads((root/'control.json').read_text())
evidence=json.loads(Path('evidence.json').read_text())
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(120)'])
record={'pid':os.getpid(),'child':child.pid,'worker':os.getppid(),'argv':sys.argv,'evidence':evidence,'marker':os.environ.get('Z_REPORT_EVALUATOR'),'api_key':os.environ.get('ANTHROPIC_API_KEY')}
(root/f"call-{os.getpid()}.json").write_text(json.dumps(record))
time.sleep(config.get('delay',0.15))
if config.get('fail_day')==evidence['date'] or config.get('fail'): sys.exit(1)
answer={'achievements':[{'title':'Implemented fixture change','contribution':'Made a recorded local change.','outcomes':[{'claim':'Changed fixture file','evidence_level':2,'evidence_refs':['file:/fixture/file.rs'],'verified':False}],'uncertainties':[],'confidence':0.9,'session_ids':[s['id'] for s in evidence['sessions']]}]}
if '-o' in sys.argv:
    Path(sys.argv[sys.argv.index('-o')+1]).write_text(json.dumps(answer));print('{"type":"turn.completed"}')
else: print(json.dumps({'structured_output':answer,'modelUsage':{'fixture':{'costUSD':0}},'num_turns':1}))
'''

class EngineTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory(prefix='z-report-test-')
        self.root=Path(self.tmp.name);self.data=self.root/'data';self.home=self.root/'home'
        self.today=datetime.date.today();self.ids=[]
        self.env=dict(os.environ,Z_REPORT_TRANSCRIPT_HOME=str(self.home),FIXTURE_ROOT=str(self.root),PATH=f'{self.root}:/usr/bin:/bin',ANTHROPIC_API_KEY='must-not-reach-evaluator')
        self.control()
        for name in ['claude','codex']:
            p=self.root/name;p.write_text(f'#!{sys.executable}\n'+FAKE);p.chmod(0o755)
        self.rpc('update_settings',patch={'claude_path':str(self.root/'claude')})
    def tearDown(self):
        for id in self.ids: self.rpc('read_cancel',id=id,check=False)
        end=time.monotonic()+3
        while time.monotonic()<end:
            if all(self.rpc('read_status',id=id)['data']['status'] not in ['queued','running'] for id in self.ids):break
            time.sleep(.05)
        self.tmp.cleanup()
    def control(self,**kw): (self.root/'control.json').write_text(json.dumps(kw))
    def rpc(self,action,check=True,**kw):
        p=subprocess.run([str(ENGINE),'--data-dir',str(self.data),'rpc'],input=json.dumps({'protocol':1,'action':action,**kw}),capture_output=True,text=True,env=self.env,timeout=10)
        if check:self.assertEqual(p.returncode,0,p.stderr)
        return json.loads(p.stdout)
    def fixture(self,id='session1',days=0,malformed=False):
        path=self.home/'.claude/projects/project'/f'{id}.jsonl';path.parent.mkdir(parents=True,exist_ok=True)
        day=self.today-datetime.timedelta(days=days)
        records=[{'type':'user','timestamp':f'{day}T12:00:00Z','message':{'role':'user','content':'Implement fixture'}},
            {'type':'assistant','timestamp':f'{day}T12:01:00Z','message':{'role':'assistant','content':[{'type':'tool_use','id':'t1','name':'Write','input':{'file_path':'/fixture/file.rs'}}]}}]
        path.write_text('\n'.join(json.dumps(r) for r in records)+ ('\n{bad' if malformed else '\n'))
        return path
    def start(self,**kw):
        result=self.rpc('read_start',owner='test',interactive=True,**kw)['data']
        if result:self.ids.append(result['read']['id']);return result['read']['id']
    def wait(self,id,heartbeat=True,timeout=10):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            r=self.rpc('read_heartbeat' if heartbeat else 'read_status',id=id,**({'owner':'test'} if heartbeat else {}))['data']
            if r['status'] not in ['running','queued']:return r
            time.sleep(.08)
        self.fail('read did not finish')
    def calls(self):return [json.loads(p.read_text()) for p in self.root.glob('call-*.json')]
    def wait_calls(self):
        end=time.monotonic()+5
        while time.monotonic()<end:
            calls=self.calls()
            if calls:return calls
            time.sleep(.03)
        self.fail('evaluator did not start')
    def stopped(self,pid):
        for _ in range(100):
            p=subprocess.run(['/bin/ps','-o','stat=','-p',str(pid)],capture_output=True,text=True)
            if not p.stdout.strip() or p.stdout.strip().startswith('Z'):return
            time.sleep(.03)
        self.fail(f'process {pid} survived')
    def sql(self,statement,values=()):
        with sqlite3.connect(self.data/'zreport.db') as db:
            db.create_function('zreport_writer_version',0,lambda:1)
            return db.execute(statement,values).fetchall()
    def test_first_use_and_empty_read(self):
        self.assertIsNone(self.start(automatic=True))
        self.assertEqual(self.wait(self.start())['status'],'completed')
        self.assertEqual(self.calls(),[])
        self.assertTrue(self.rpc('overview')['data']['initialized'])
        self.assertIsNone(self.start(automatic=True))
    def test_review_edit_approve_search_export_and_unchanged_read(self):
        self.fixture();r=self.wait(self.start());self.assertEqual(r['status'],'completed')
        c=self.rpc('candidates',status='pending')['data'][0]
        self.rpc('edit_candidate',id=c['id'],revision=c['revision'],title='Changed title',contribution='Edited by human',outcomes=c['outcomes'])
        stale=self.rpc('approve_candidate',id=c['id'],revision=0,check=False);self.assertEqual(stale['error']['code'],'conflict')
        self.rpc('approve_candidate',id=c['id'],revision=1);self.rpc('approve_candidate',id=c['id'],revision=1)
        journal=self.rpc('journal',**{'from':str(self.today),'to':str(self.today),'query':'Changed'})['data'];self.assertEqual(len(journal),1)
        export=self.rpc('export',**{'from':str(self.today),'to':str(self.today)})['data'];self.assertIn('Changed title',export['markdown'])
        path=self.root/'journal.md';self.rpc('save_export',**{'from':str(self.today),'to':str(self.today),'path':str(path)})
        self.assertEqual(path.read_text(),export['markdown'])
        self.assertFalse(self.rpc('save_export',check=False,**{'from':str(self.today),'to':str(self.today),'path':str(path)})['ok'])
        self.assertEqual(self.wait(self.start())['status'],'completed');self.assertEqual(len(self.calls()),1)
        call=self.calls()[0];self.assertEqual(call['marker'],'1');self.assertIsNone(call['api_key']);self.assertIn('--safe-mode',call['argv']);self.stopped(call['child'])
    def test_incomplete_scan_does_not_initialize(self):
        self.fixture(malformed=True);read=self.wait(self.start());self.assertEqual(read['status'],'failed');self.assertFalse(read['scan']['complete'])
        self.assertFalse(self.rpc('overview')['data']['initialized']);self.assertEqual(self.calls(),[])
    def test_partial_failure_keeps_first_day_and_remaining_pending(self):
        self.fixture('old',1);self.fixture('new');self.control(fail_day=str(self.today))
        read=self.wait(self.start());self.assertEqual((read['status'],read['completed_days']),('failed',1))
        self.assertEqual(len(self.rpc('candidates',status='pending')['data']),1)
        self.assertEqual(self.sql('SELECT id FROM sessions WHERE content_hash<>evaluated_hash'),[('new',)])
        self.assertFalse(self.rpc('overview')['data']['initialized'])
    def test_cancel_stops_process_tree_and_leaves_evidence_pending(self):
        self.fixture();self.control(delay=120);id=self.start();call=self.wait_calls()[0]
        self.rpc('read_cancel',id=id);self.assertEqual(self.wait(id)['status'],'cancelled')
        self.stopped(call['pid']);self.stopped(call['child']);self.assertEqual(len(self.sql('SELECT id FROM sessions WHERE content_hash<>evaluated_hash')),1)
    def test_owner_loss_expires_lease(self):
        self.fixture();self.control(delay=120);id=self.start();call=self.wait_calls()[0]
        self.sql("UPDATE read_cycles SET heartbeat_at='2000-01-01T00:00:00Z' WHERE id=?",(id,))
        self.assertEqual(self.wait(id,heartbeat=False)['status'],'cancelled');self.stopped(call['child'])
    def test_competing_launches_have_one_worker(self):
        self.fixture();self.control(delay=120)
        with ThreadPoolExecutor(max_workers=4) as pool:
            results=list(pool.map(lambda _:self.rpc('read_start',owner='test',interactive=True,check=False),range(4)))
        owners=[r['data'] for r in results if r['ok'] and r['data']['owned']]
        self.assertEqual(len(owners),1);id=owners[0]['read']['id'];self.ids.append(id)
        self.wait_calls();self.assertEqual(len(self.calls()),1)
        self.rpc('read_cancel',id=id);self.wait(id)
    def test_worker_crash_kills_evaluator_before_recovery(self):
        self.fixture();self.control(delay=120);id=self.start();call=self.wait_calls()[0]
        os.kill(call['worker'],signal.SIGKILL);self.stopped(call['pid']);self.stopped(call['child'])
        self.assertEqual(self.wait(id,heartbeat=False)['status'],'interrupted')
        self.control();self.assertEqual(self.wait(self.start())['status'],'completed')
    def test_changes_during_evaluation_remain_pending(self):
        path=self.fixture();self.control(delay=.7);id=self.start();self.wait_calls()
        with path.open('a') as f:f.write(json.dumps({'type':'assistant','timestamp':f'{self.today}T12:02:00Z','message':{'role':'assistant','content':[{'type':'tool_use','id':'t2','name':'Write','input':{'file_path':'/fixture/second.rs'}}]}})+'\n')
        self.rpc('scan');self.assertEqual(self.wait(id)['status'],'completed')
        self.assertEqual(len(self.sql('SELECT id FROM sessions WHERE content_hash<>evaluated_hash')),1)
    def test_discard_restore_merge(self):
        self.fixture('old',1);self.fixture('new');self.wait(self.start())
        cards=self.rpc('candidates',status='pending')['data'];c=cards[0]
        self.rpc('discard_candidate',id=c['id'],revision=c['revision']);d=self.rpc('candidates',status='discarded')['data'][0]
        self.rpc('restore_candidate',id=d['id'],revision=d['revision']);cards=self.rpc('candidates',status='pending')['data']
        self.rpc('merge_candidates',ids=[c['id'] for c in cards],revisions=[c['revision'] for c in cards])
        self.assertEqual(len(self.rpc('candidates',status='pending')['data']),1)
    def test_privacy_setting_removes_existing_excerpts(self):
        self.fixture();self.rpc('scan')
        self.assertIn('Implement fixture',self.sql('SELECT facts FROM sessions')[0][0])
        self.rpc('update_settings',patch={'retain_prompts':False})
        facts=json.loads(self.sql('SELECT facts FROM sessions')[0][0]);self.assertEqual(facts['prompts'],[]);self.assertIsNone(facts['title'])
        self.wait(self.start());self.assertEqual(self.calls()[0]['evidence']['sessions'][0]['user_prompts'],[])
    def test_incomplete_delegate_fails_scan(self):
        path=self.fixture();delegate=path.with_suffix('')/'subagents/broken.jsonl';delegate.parent.mkdir(parents=True);delegate.write_text('{broken')
        self.assertEqual(self.wait(self.start())['status'],'failed');self.assertEqual(self.calls(),[])
    def test_excluded_directory_never_reaches_evaluator(self):
        path=self.fixture();lines=[json.loads(line) for line in path.read_text().splitlines()];lines[0]['cwd']='/private/excluded/project';path.write_text('\n'.join(map(json.dumps,lines)))
        self.rpc('update_settings',patch={'excluded_repos':['/private/excluded']})
        self.assertEqual(self.wait(self.start())['status'],'completed');self.assertEqual(self.calls(),[])
    def test_existing_older_journal_is_left_for_the_desktop_app(self):
        data=self.root/'legacy';data.mkdir()
        with sqlite3.connect(data/'zreport.db') as db:db.execute('CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL)')
        p=subprocess.run([str(ENGINE),'--data-dir',str(data),'rpc'],input=json.dumps({'protocol':1,'action':'overview'}),capture_output=True,text=True,env=self.env,timeout=10)
        self.assertEqual((p.returncode,json.loads(p.stdout)['error']['code']),(5,'incompatible'))
        with sqlite3.connect(data/'zreport.db') as db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone(),(0,));db.execute("INSERT INTO kv VALUES('k','v')")
    def test_headless_automatic_request_never_initializes(self):
        self.fixture();self.assertIsNone(self.rpc('read_start',owner='test',automatic=True,interactive=False)['data']);self.assertEqual(self.calls(),[])

if __name__=='__main__':unittest.main()
