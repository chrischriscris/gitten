import './style.css';

// Interactive desktop design reference. Repository operations use in-memory fixtures.
const icons = {
 branch:'<circle cx="6" cy="5" r="2"/><circle cx="6" cy="19" r="2"/><circle cx="18" cy="5" r="2"/><path d="M6 7v10M18 7c0 7-12 3-12 8"/>',
 changes:'<rect x="5" y="3" width="14" height="18" rx="3"/><path d="M9 8h6M9 12h6M9 16h3"/>',
 history:'<path d="M3 10a9 9 0 1 1 1 7M3 4v6h6M12 7v5l3 2"/>',
 stash:'<path d="m3 7 9-4 9 4-9 4-9-4Zm0 0v10l9 4 9-4V7M12 11v10"/>',
 search:'<circle cx="10" cy="10" r="6"/><path d="m15 15 5 5"/>',
 settings:'<path d="M4 7h16M4 17h16"/><circle cx="9" cy="7" r="3"/><circle cx="16" cy="17" r="3"/>',
 check:'<path d="m5 12 4 4L19 6"/>',
 arrow:'<path d="M12 19V5m-5 5 5-5 5 5"/>',
 file:'<path d="M14 3H5v18h14V8l-5-5Zm0 0v5h5"/>',
 chevron:'<path d="m9 5 7 7-7 7"/>',
 command:'<path d="M8 8h8v8H8z"/><path d="M8 8H5a3 3 0 1 1 3-3v3Zm8 0V5a3 3 0 1 1 3 3h-3ZM8 16v3a3 3 0 1 1-3-3h3Zm8 0h3a3 3 0 1 1-3 3v-3Z"/>',
};
const icon = name => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${icons[name] || icons.file}</svg>`;
const files = [
 {name:'scroll.rs',dir:'shell/src/diff',added:18,removed:6, status:'Modified', hunks:[[
 [' ','    let delta = event.delta.pixel_delta(line_height);'],
 ['-','    self.scroll_top += delta.y;'],
 ['-','    self.scroll_left += delta.x;'],
 ['+','    let axis = self.ongoing_scroll.axis(event);'],
 ['+','    if axis == Axis::Horizontal {'],
 ['+','        self.offset.pan(delta.x, self.max_offset);'],
 ['+','        cx.stop_propagation();'],
 ['+','        return;'],
 ['+','    }'],
 [' ','    self.list.scroll_by(delta.y);'],
 ],[
 [' ','impl ScrollbarHandle for DiffScroll {'],
 [' ','    fn offset(&self) -> Point<Pixels> {'],
 ['-','        point(px(0.), px(0.))'],
 ['+','        point(-self.offset.get(), px(0.))'],
 [' ','    }'],
 [' ','}'],
 ]]},
 {name:'view.rs',dir:'shell/src/diff',added:8,removed:3,status:'Modified',hunks:[[
 [' ','    div()'],[' ','        .size_full()'],['-','        .child(self.render_text(cx))'],['+','        .child(self.render_gutter(cx))'],['+','        .child(div().overflow_hidden()'],['+','            .child(self.render_text(cx)))'],[' ','}'],
 ]]},
 {name:'scroll.rs',dir:'core/src',added:24,removed:0,status:'New file',hunks:[[
 ['+','/// A bounded text offset, independent of its viewport.'],['+','#[derive(Default)]'],['+','pub struct ScrollOffset {'],['+','    position: f32,'],['+','}'],['+',''],['+','impl ScrollOffset {'],['+','    pub fn pan(&mut self, delta: f32, max: f32) {'],['+','        self.position = (self.position + delta).clamp(0., max);'],['+','    }'],['+','}'],
 ]]},
 {name:'scrolling.md',dir:'docs/decisions',added:12,removed:0,status:'New file',hunks:[[
 ['+','# Keep the gutter still'],['+',''],['+','The line numbers belong to the viewport.'],['+','The text belongs to the document.'],['+',''],['+','A horizontal gesture pans only the text.'],['+','Lock the axis until the gesture ends.'],
 ]]},
];
const state = {file:0,staged:new Set(),view:'changes',message:'',description:'',split:false,dark:false,filter:'',notice:'',commit:0,published:false,history:0};
const root = document.querySelector('#root');
const esc = value => String(value).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const total = () => files.reduce((sum,f)=>sum+f.hunks.length,0);
const fullyStaged = i => files[i].hunks.every((_,h)=>state.staged.has(`${i}:${h}`));
const stagedFiles = () => files.filter((_,i)=>files[i].hunks.some((_,h)=>state.staged.has(`${i}:${h}`))).length;
function button(action,label,cls='',attrs='') {return `<button class="${cls}" data-action="${action}" ${attrs}>${label}</button>`;}
function fileList() {
 const matching = state.commit ? [] : files.map((f, i) => ({...f, index:i}))
  .filter(f => `${f.dir}/${f.name}`.toLowerCase().includes(state.filter.toLowerCase()));
 const stageCount = i => files[i].hunks.filter((_, h) => state.staged.has(`${i}:${h}`)).length;
 const groupFor = f => f.dir;
 const groups = [...new Set(matching.map(groupFor))];
 const rows = groups.map(group => {
  const members = matching.filter(f => groupFor(f) === group);
  if (!members.length) return '';
  return `<section class="file-group"><div class="file-group-heading"><span>${icon('chevron')}${group}</span><span>${members.length}</span></div>${members.map(f => {
   const i = f.index, count = stageCount(i), staged = fullyStaged(i), partial = count > 0 && !staged;
   return `<div class="file-row ${state.file===i?'selected':''}">${button('stage-file',staged?icon('check'):partial?'−':'',`stage-check ${staged?'checked':''} ${partial?'partial':''}`,`data-index="${i}" aria-label="${staged?'Unstage':'Stage'} ${f.dir}/${f.name}" aria-pressed="${partial?'mixed':staged}"`)}${button('file',`<span class="file-label"><strong>${f.name}</strong></span>${partial?`<span class="partial-count">${count}/${f.hunks.length}</span>`:''}<span class="file-symbol ${f.status==='New file'?'new':''}">${f.status==='New file'?'A':'M'}</span>`,'file-select',`data-index="${i}" aria-pressed="${state.file===i}" title="${f.dir}/${f.name}"`)}</div>`;
  }).join('')}</section>`;
 }).join('');
 return `<section class="file-list detail-files"><div class="list-heading"><strong>Changed files <span class="muted">${state.commit?0:files.length}</span></strong>${button('stage-all',state.staged.size===total()?'Unstage all':'Stage all','text-button',state.commit?'disabled':'')}</div><label class="filter">${icon('search')}<input id="file-filter" placeholder="Filter files…" value="${esc(state.filter)}" aria-label="Filter files"><kbd>/</kbd></label><div class="file-items">${rows||`<p class="empty-small">${state.commit?'Working tree clean':'No matching files'}</p>`}</div><div class="list-foot">${state.staged.size} of ${total()} hunks staged</div></section>`;
}
function highlight(code) {return esc(code).replace(/\b(let|if|return|impl|for|fn|pub|struct|mut|self)\b/g,'<span class="syntax">$1</span>');}
function diff() {const f=files[state.file];return `<section class="diff"><div class="diff-toolbar"><div class="breadcrumb">${icon('file')}<span>${f.dir}/</span><strong>${f.name}</strong></div><div class="segmented">${button('unified','Unified',!state.split?'chosen':'',`aria-pressed="${!state.split}"`)}${button('split','Split',state.split?'chosen':'',`aria-pressed="${state.split}"`)}</div></div><div class="diff-summary"><span><strong>${f.status}</strong> <span class="muted">in working tree</span></span><span><b class="green">+${f.added}</b> <b class="red">−${f.removed}</b></span></div><div class="code-scroll">${f.hunks.map((rows,h)=>`<section class="hunk"><div class="hunk-heading"><span>@@ ${h?'-86,6 +92,6':'-42,5 +42,10'} @@ <span class="hunk-context">${state.file===0?'handle_scroll':f.name}</span></span>${button('stage-hunk',state.staged.has(`${state.file}:${h}`)?icon('check')+' Staged':'＋ Stage hunk',state.staged.has(`${state.file}:${h}`)?'hunk-staged':'',`data-hunk="${h}" aria-pressed="${state.staged.has(`${state.file}:${h}`)}"`)}</div><div class="code-table ${state.split?'split':''}">${codeRows(rows,h)}</div></section>`).join('')}<div class="end-of-diff">End of changes <span>·</span> ${f.hunks.length} ${f.hunks.length===1?'hunk':'hunks'}</div></div></section>`;}
function codeRows(rows,h) {let old=h?86:42,newLine=old; if(!state.split)return rows.map(([kind,code])=>`<div class="code-row ${kind==='+'?'addition':kind==='-'?'deletion':''}"><span class="line-no">${kind==='+'?'':old++}</span><span class="line-no">${kind==='-'?'':newLine++}</span><span class="sign">${kind===' '?'':kind}</span><code>${highlight(code)||' '}</code></div>`).join('');const out=[];for(let i=0;i<rows.length;){if(rows[i][0]===' '){out.push([rows[i],rows[i]]);i++;}else{const removed=[],added=[];while(i<rows.length&&rows[i][0]!==' '){(rows[i][0]==='-'?removed:added).push(rows[i++]);}for(let j=0;j<Math.max(removed.length,added.length);j++)out.push([removed[j],added[j]]);}}return out.map(pair=>`<div class="split-row">${pair.map((r,side)=>`<div class="code-row ${r?(r[0]==='+'?'addition':r[0]==='-'?'deletion':''):'missing'}"><span class="line-no">${r?(side?newLine++:old++):''}</span><span class="sign">${r&&r[0]!==' '?r[0]:''}</span><code>${r?highlight(r[1])||' ':''}</code></div>`).join('')}</div>`).join('');}
function composer() {return `<section class="composer"><div class="composer-heading"><div><h2>Commit</h2></div><span class="staged-pill">${stagedFiles()} files staged</span></div><label class="field-label" for="commit-title">Summary</label><input id="commit-title" aria-label="Commit summary" placeholder="Commit summary" maxlength="120" value="${esc(state.message)}"><label class="field-label" for="commit-description">Description <span>Optional</span></label><textarea id="commit-description" aria-label="Commit description" placeholder="Description (optional)">${esc(state.description)}</textarea><div class="commit-bottom"><span>${state.staged.size?`${state.staged.size} hunks staged`:'No staged changes'}</span>${button('commit','Commit'+icon('chevron'),'primary',(!state.message.trim()||!state.staged.size?'disabled':'')+' title="Commit staged changes · ⌘ Enter"')}</div></section>`;}
const commits = [{title:'Keep line numbers fixed while panning',hash:'d2f8a31',time:'24 min ago',who:'You',file:0},{title:'Share wrap budgets across layouts',hash:'91be82c',time:'1 hour ago',who:'You',file:1},{title:'Cache diff preparation by blob pair',hash:'6fc043a',time:'Yesterday',who:'Maya',file:2},{title:'Document the row layout contract',hash:'b8a94e1',time:'Yesterday',who:'Maya',file:3}];
// Real lane geometry, not a single rail: main runs down lane 0 and
// fix/scroll-drift forks off it at c2 into lane 1, its tip at c0 and its
// uncommitted row above. The desktop draws the same shape from core's plan.
const LANE_W = 14;
const laneX = lane => 7 + lane * LANE_W;
const MAIN_LANE = '#5a947c';
const BRANCH_LANE = '#9a88ba';
const lanePath = (lane, color) => `<path d="M${laneX(lane)} 0V100" stroke="${color}" vector-effect="non-scaling-stroke"/>`;
const forkPath = `<path d="M${laneX(0)} 50C${laneX(0)} 25 ${laneX(1)} 25 ${laneX(1)} 0" stroke="${BRANCH_LANE}" fill="none" vector-effect="non-scaling-stroke"/>`;
function graphGutter(paths, nodeLane, nodeClass = '') {
  const w = 2 * LANE_W;
  return `<span class="graph-gutter" style="width:${w}px"><svg class="graph-lanes" viewBox="0 0 ${w} 100" preserveAspectRatio="none" aria-hidden="true">${paths.join('')}</svg><span class="graph-node ${nodeClass}" style="left:${laneX(nodeLane)}px"></span></span>`;
}
function timeline() {
  const trunk = lanePath(0, MAIN_LANE);
  const branch = lanePath(1, BRANCH_LANE);
  const row = (action, gutter, copy, cls, attrs = '') => button(action, `${gutter}<span class="timeline-copy">${copy}</span>`, `timeline-row ${cls}`, attrs);
  return `<section class="timeline"><div class="list-heading"><strong>Branch history</strong><span class="muted">${icon('branch')}</span></div>` +
    row('changes', graphGutter([trunk, branch], 1, 'branch current'), `<strong>Uncommitted changes</strong><small>Working tree · ${state.commit?'clean':'4 files'}</small>`, state.view==='changes'?'selected':'') +
    `<div class="timeline-date">Today</div>` +
    commits.map((c, i) => {
      const gutter = i < 2 ? graphGutter([trunk, branch], 1, 'branch')
        : i === 2 ? graphGutter([trunk, forkPath], 0)
          : graphGutter([trunk], 0);
      const ref = i === 0 ? '<span class="ref-tag">fix/scroll-drift</span>' : i === 2 ? '<span class="ref-tag neutral">main · origin/main</span>' : '';
      return row('select-commit', gutter, `<strong>${c.title}</strong><small>${c.who} · ${c.time} <code>${c.hash}</code></small>${ref}`, state.view==='history'&&state.history===i?'selected':'', `data-index="${i}"`);
    }).join('') +
    `</section>`;
}
function historyDetail(){const c=commits[state.history];return `<section class="history-detail"><div class="detail-heading"><span class="section-caption">COMMIT ${c.hash}</span><h2>${c.title}</h2><p><span class="avatar small">${c.who==='You'?'C':'M'}</span> ${c.who} <span class="muted">committed ${c.time}</span></p></div><div class="history-body"><div class="historical-file">${icon('file')} ${files[c.file].dir}/${files[c.file].name}<span class="green">+${files[c.file].added}</span></div><div class="hunk-heading">Commit diff</div><div class="code-table">${codeRows(files[c.file].hunks[0],0)}</div></div><div class="history-detail-foot">Parent <code>${commits[state.history+1]?.hash||'60d3fa2'}</code><span>Authored on fix/scroll-drift</span></div></section>`;}
function clean(){return `<section class="clean-state">${icon('check')}<h2>Working tree clean</h2><p>Branch: <strong>fix/scroll-drift</strong></p><p class="muted">${esc(state.message)} · ${state.published?'Pushed to origin':'Committed locally'}</p>${button('reset','Reset demo','secondary')}</section>`;}
function stagedSummary() {
 const items = files.map((f, i) => {
  const count = f.hunks.filter((_, h) => state.staged.has(`${i}:${h}`)).length;
  if (!count) return '';
  return `<div class="index-file">${icon('check')}<div><strong>${f.name}</strong><small>${f.dir}</small></div><span>${count}/${f.hunks.length} hunks</span></div>`;
 }).join('');
 return `<section class="index-summary"><div class="section-caption">STAGED FILES <span>${stagedFiles()}</span></div>${items || '<p class="index-empty">No staged changes</p>'}</section>`;
}
function unifiedSidebar() {
 return `<aside class="unified-sidebar"><div class="repo-identity"><span class="repo-mark">${icon('branch')}</span><div><strong>gitten</strong><small>~/Projects/gitten</small></div></div><nav class="unified-navigation" aria-label="Repository">${button('changes',icon('changes')+'Changes',`nav-item ${state.view==='changes'?'active':''}`)}${button('history',icon('history')+'History',`nav-item ${state.view==='history'?'active':''}`)}</nav><div class="sidebar-utilities">${button('branches',icon('branch')+'Branches','text-button')}${button('stash',icon('stash')+'Stashes · 1','text-button')}</div>${state.view==='changes'?fileList():`<div class="history-sidebar-note"><span class="section-caption">CURRENT BRANCH</span><p>${icon('branch')} fix/scroll-drift</p></div>`}<div class="unified-bottom"><span class="tiny-dot"></span> Local workspace <span>gitten</span></div></aside>`;
}
function content() {
 if(state.view==='history') return `<div class="history-layout">${timeline()}${historyDetail()}</div>`;
 if(state.commit) return clean();
 return `<div class="inspector-layout">${diff()}<aside class="commit-inspector"><div class="inspector-heading"><h2>Commit</h2></div>${stagedSummary()}${composer()}</aside></div>`;
}
function render(){root.innerHTML=`<div class="guide ${state.dark?'dark':''}"><main class="app-window" aria-label="gitten"><header class="titlebar"><div class="traffic-lights" aria-hidden="true"><i></i><i></i><i></i></div><div class="branch-control">${icon('branch')}<strong>fix/scroll-drift</strong><span class="branch-base">from main</span></div><div class="toolbar-actions">${button('search',icon('search')+'<span>Commands</span><kbd>⌘ K</kbd>','command-button')}${button('publish',icon('arrow')+(state.published?'Published':'Push '+(state.commit?3:2)),'secondary',state.published?'disabled':'')}</div></header><div class="window-body">${unifiedSidebar()}<div class="main-area"><div class="workspace-header"><div><h2>${state.view==='history'?'History':'Changes'}</h2><span>${state.view==='history'?'fix/scroll-drift':state.commit?'Working tree clean':'4 files changed'} <span class="separator">·</span> ${state.view==='history'?'4 recent commits':state.commit?'No uncommitted changes':'62 additions, 9 deletions'}</span></div><span class="working-copy"><span class="tiny-dot"></span> Working copy</span></div>${content()}</div></div><footer class="statusbar"><span><span class="tiny-dot"></span> ${state.published?'Last push just now':'Last fetched 2 minutes ago'} <span class="separator">·</span> origin</span><span>${state.staged.size} staged hunks ${button('search','Keyboard shortcuts <kbd>⌘ K</kbd>','text-button')}</span></footer></main><div class="toast ${state.notice?'visible':''}" role="status">${esc(state.notice)}</div><dialog id="dialog"></dialog></div>`;}
let noticeTimer;
function notify(message){state.notice=message;const toast=document.querySelector('.toast');toast.textContent=message;toast.classList.add('visible');clearTimeout(noticeTimer);noticeTimer=setTimeout(()=>{state.notice='';toast.classList.remove('visible');},3500);}
let dialogReturnFocus;
function openDialog(title,body){dialogReturnFocus=document.activeElement;const dialog=document.querySelector('#dialog');dialog.innerHTML=`<div class="dialog-heading"><h2>${title}</h2>${button('close-dialog','×','close','aria-label="Close dialog"')}</div>${body}`;dialog.showModal();dialog.addEventListener('close',()=>dialogReturnFocus?.focus(),{once:true});}
function commandDialog(){openDialog('Commands',`<label class="filter command-search">${icon('search')}<input id="command-search" autofocus placeholder="Search commands…" aria-label="Find a command"></label><div class="command-list">${[['changes','Open changes','⌘ 1'],['history','Open history','⌘ 2'],['stage-all','Stage / unstage all files',''],['branches','View branches',''],['stash','View stash',''],['theme','Toggle appearance',''],['reset','Reset demo','']].map(([a,label,key])=>button(a,`<span>${label}</span><kbd>${key}</kbd>`,'command-item',`data-command="${label.toLowerCase()}"`)).join('')}</div><p class="dialog-hint">Tab to navigate · Enter to run · Esc to close</p>`);}
root.addEventListener('input',event=>{const el=event.target;if(el.id==='commit-title'){state.message=el.value;document.querySelector('[data-action="commit"]').disabled=!state.message.trim()||!state.staged.size;}if(el.id==='commit-description')state.description=el.value;if(el.id==='file-filter'){state.filter=el.value;const pos=el.selectionStart;render();const input=document.querySelector('#file-filter');input.focus();input.setSelectionRange(pos,pos);}if(el.id==='command-search')document.querySelectorAll('[data-command]').forEach(row=>row.hidden=!row.dataset.command.includes(el.value.toLowerCase()));});
root.addEventListener('click',event=>{const el=event.target.closest('[data-action]');if(!el)return;const action=el.dataset.action;const index=Number(el.dataset.index);const dialog=document.querySelector('#dialog');if(dialog.open&&action!=='close-dialog')dialog.close();switch(action){case 'theme':state.dark=!state.dark;break;case 'file':state.file=index;break;case 'changes':case 'history':state.view=action;break;case 'select-commit':state.view='history';state.history=index;break;case 'stage-file':{const staged=fullyStaged(index);files[index].hunks.forEach((_,h)=>state.staged[staged?'delete':'add'](`${index}:${h}`));break;}case 'stage-hunk':{const key=`${state.file}:${el.dataset.hunk}`;state.staged[state.staged.has(key)?'delete':'add'](key);break;}case 'stage-all':if(state.commit)return;if(state.staged.size===total())state.staged.clear();else files.forEach((f,i)=>f.hunks.forEach((_,h)=>state.staged.add(`${i}:${h}`)));break;case 'unified':state.split=false;break;case 'split':state.split=true;break;case 'search':commandDialog();return;case 'close-dialog':dialog.close();return;case 'commit':if(!state.message.trim()||!state.staged.size)return;openDialog('Commit staged changes',`<p class="dialog-copy">Branch: <strong>fix/scroll-drift</strong></p><div class="commit-preview"><strong>${esc(state.message)}</strong><p>${esc(state.description)}</p><span>${stagedFiles()} files · ${state.staged.size} hunks staged</span></div>${button('confirm-commit','Commit','primary')}`);return;case 'confirm-commit':if(state.staged.size!==total()){notify('Commit created');openDialog('Commit created',`<p class="dialog-copy"><strong>${esc(state.message)}</strong><br>${state.staged.size} hunks committed · ${total()-state.staged.size} hunks remaining</p><p class="dialog-hint">End of demo</p>${button('reset','Reset demo','primary')}`);return;}state.commit++;state.staged.clear();state.published=false;break;case 'reset':state.commit=0;state.staged.clear();state.message='';state.description='';state.published=false;break;case 'publish':openDialog('Push to origin',`<p class="dialog-copy">${state.commit?3:2} commits<br><strong>fix/scroll-drift → origin/fix/scroll-drift</strong></p>${button('confirm-push','Push','primary')}`);return;case 'confirm-push':state.published=true;break;case 'branches':openDialog('Branches',`<p class="dialog-copy">Current branch: <strong>fix/scroll-drift</strong></p><div class="branch-option">${icon('branch')} fix/scroll-drift <span>2 ahead · current</span></div><div class="branch-option">${icon('branch')} main <span>up to date with origin/main</span></div><p class="dialog-hint">Read-only preview</p>`);return;case 'stash':openDialog('Stashes',`<div class="commit-preview"><strong>WIP: explore compact toolbar</strong><p>stash@{0} · saved yesterday on main</p><span>3 files changed</span></div><p class="dialog-hint">Read-only preview</p>`);return;default:return;}render();const replacement=[...document.querySelectorAll('[data-action]')].find(b=>b.dataset.action===action&&b.dataset.index===el.dataset.index&&b.dataset.hunk===el.dataset.hunk);replacement?.focus({preventScroll:true});});
document.addEventListener('keydown',event=>{const typing=event.target.matches('input,textarea,[contenteditable]');if(document.querySelector('#dialog').open)return;if((event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='k'){event.preventDefault();commandDialog();return;}if((event.metaKey||event.ctrlKey)&&event.key==='Enter'){event.preventDefault();document.querySelector('[data-action="commit"]')?.click();return;}if(typing)return;if((event.metaKey||event.ctrlKey)&&['1','2'].includes(event.key)){event.preventDefault();state.view=event.key==='1'?'changes':'history';render();return;}if(event.key==='/'&&document.querySelector('#file-filter')){event.preventDefault();document.querySelector('#file-filter').focus();}});

render();
