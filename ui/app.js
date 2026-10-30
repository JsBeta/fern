'use strict';

const API = '/_api';
const CHUNK = 4 * 1024 * 1024;
const TEXT_LIMIT = 2 * 1024 * 1024;
// Reserved hash for the recycle bin. `.Trash` itself is never listed by the
// file API, so the bin needs its own route.
const TRASH_PATH = '/__trash';

const state = {
  path: '/',
  session: null,
  entries: [],
  selected: new Set(),
  clip: null,
  busy: false,
  editorDirty: false,
  trash: null,
};

const el = (id) => document.getElementById(id);
const app = document.querySelector('.app');

// SVGElement 没有 hidden 属性，必须改特性才能让 [hidden] 规则生效
function setHidden(node, hidden) {
  if (hidden) node.setAttribute('hidden', '');
  else node.removeAttribute('hidden');
}

function sleep(ms) { return new Promise((r) => setTimeout(r, ms)); }

async function api(path, options) {
  const opts = Object.assign({ credentials: 'same-origin' }, options || {});
  const res = await fetch(API + path, opts);
  const text = await res.text();
  let data = null;
  if (text) {
    try { data = JSON.parse(text); } catch (e) { data = { message: text }; }
  }
  if (!res.ok) {
    const err = new Error((data && (data.message || data.error)) || res.statusText);
    err.status = res.status;
    err.data = data;
    throw err;
  }
  return data;
}

function post(path, body) {
  return api(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body || {}),
  });
}

function can(letter) {
  return !!state.session && state.session.perms.indexOf(letter) !== -1;
}

// The recycle bin can be switched off server side (Trash.Enabled 0); the UI
// then hides every bin affordance and delete warnings become permanent.
function trashOn() {
  return !!(state.session && state.session.trashEnabled);
}

function toast(message) {
  const node = el('toast');
  node.textContent = message;
  node.hidden = false;
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => { node.hidden = true; }, 2600);
}

function fmtSize(bytes) {
  if (bytes < 1024) return bytes + ' B';
  const units = ['KB', 'MB', 'GB', 'TB', 'PB'];
  let value = bytes / 1024;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) { value /= 1024; i++; }
  return value.toFixed(value < 10 ? 1 : 0) + ' ' + units[i];
}

function fmtTime(secs) {
  if (!secs) return '';
  const d = new Date(secs * 1000);
  const p = (n) => String(n).padStart(2, '0');
  return d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) +
    ' ' + p(d.getHours()) + ':' + p(d.getMinutes());
}

function joinPath(dir, name) {
  return dir === '/' ? '/' + name : dir + '/' + name;
}

function isTrashPath(p) {
  return (p || '') === TRASH_PATH;
}

function parentPath(p) {
  if (p === '/' || !p) return '/';
  const trimmed = p.replace(/\/+$/, '');
  const idx = trimmed.lastIndexOf('/');
  return idx <= 0 ? '/' : trimmed.slice(0, idx);
}

function encodePath(p) {
  return p.split('/').map((s) => (s ? encodeURIComponent(s) : s)).join('/');
}

// ---------------------------------------------------------------- navigation

function setPath(path) {
  state.path = path || '/';
  state.selected.clear();
  location.hash = '#' + encodePath(state.path);
  load();
}

function pathFromHash() {
  const raw = location.hash.replace(/^#/, '');
  if (!raw) return '/';
  try {
    const parts = raw.split('/').map((s) => (s ? decodeURIComponent(s) : s));
    const joined = parts.join('/');
    return joined.startsWith('/') ? joined : '/' + joined;
  } catch (e) {
    return '/';
  }
}

async function load() {
  if (state.busy) return;
  if (isTrashPath(state.path)) {
    if (!trashOn()) {
      state.path = '/';
      location.hash = '#/';
      return load();
    }
    return loadTrash();
  }
  state.busy = true;
  try {
    const data = await api('/list?path=' + encodeURIComponent(state.path));
    state.entries = data.entries || [];
    state.path = data.path || state.path;
    renderCrumbs();
    renderList();
    el('status').textContent = data.writable ? '可写' : '只读';
    el('count').textContent = state.entries.length + ' 项';
  } catch (e) {
    if (e.status === 401 || e.status === 403) {
      toast('无权访问该目录');
      state.path = '/';
    } else if (e.status === 404) {
      toast('目录不存在');
      state.path = '/';
    } else {
      toast('加载失败：' + e.message);
    }
    if (state.path !== '/') { state.path = '/'; state.busy = false; return load(); }
  } finally {
    state.busy = false;
  }
  refreshToolbar();
}

async function loadTrash() {
  if (state.busy) return;
  state.busy = true;
  let failed = false;
  try {
    const data = await api('/trash');
    state.trash = data;
    state.entries = [];
    renderCrumbs();
    renderList();
    el('status').textContent = data.enabled
      ? '回收站保留 ' + data.maxDays + ' 天'
      : '回收站已停用，删除即永久移除';
    el('count').textContent = (data.total || 0) + ' 项 · ' + fmtSize(data.bytes || 0);
  } catch (e) {
    failed = true;
    toast('无法加载回收站：' + e.message);
  } finally {
    state.busy = false;
  }
  if (failed) {
    state.path = '/';
    location.hash = '#/';
    return load();
  }
  refreshToolbar();
}

function renderCrumbs() {
  const wrap = el('crumbs');
  wrap.textContent = '';
  const rootLink = document.createElement('a');
  rootLink.href = '#/';
  rootLink.textContent = '根目录';
  rootLink.onclick = (ev) => { ev.preventDefault(); setPath('/'); };
  wrap.appendChild(rootLink);
  if (isTrashPath(state.path)) {
    const sep = document.createElement('span');
    sep.className = 'sep';
    sep.textContent = '/';
    wrap.appendChild(sep);
    const current = document.createElement('span');
    current.className = 'current';
    current.textContent = '回收站';
    wrap.appendChild(current);
    return;
  }
  const parts = state.path.split('/').filter(Boolean);
  let acc = '';
  for (const part of parts) {
    acc += '/' + part;
    const sep = document.createElement('span');
    sep.className = 'sep';
    sep.textContent = '/';
    wrap.appendChild(sep);
    const link = document.createElement('a');
    link.href = '#' + encodePath(acc);
    link.textContent = part;
    const target = acc;
    link.onclick = (ev) => { ev.preventDefault(); setPath(target); };
    wrap.appendChild(link);
  }
}

const SVG_DIR = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/></svg>';
const SVG_FILE = '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/></svg>';

function renderList() {
  const list = el('list');
  if (isTrashPath(state.path)) { renderTrashList(); return; }
  list.className = 'list';
  list.textContent = '';
  el('empty').textContent = '此目录为空';
  el('empty').hidden = state.entries.length > 0;
  state.entries.forEach((entry) => {
    const row = document.createElement('div');
    row.className = 'row' + (entry.isDir ? ' dir' : '') +
      (state.selected.has(entry.name) ? ' selected' : '');

    const box = document.createElement('input');
    box.type = 'checkbox';
    box.checked = state.selected.has(entry.name);
    box.onclick = (ev) => {
      ev.stopPropagation();
      if (box.checked) state.selected.add(entry.name); else state.selected.delete(entry.name);
      row.classList.toggle('selected', box.checked);
      refreshToolbar();
    };

    const nameCell = document.createElement('div');
    nameCell.className = 'name';
    const icon = document.createElement('span');
    icon.className = 'icon';
    icon.innerHTML = entry.isDir ? SVG_DIR : SVG_FILE;
    const label = document.createElement('span');
    label.textContent = entry.name;
    nameCell.appendChild(icon);
    nameCell.appendChild(label);
    nameCell.onclick = () => {
      if (entry.isDir) setPath(joinPath(state.path, entry.name));
      else preview(entry);
    };

    const size = document.createElement('div');
    size.className = 'size';
    size.textContent = entry.isDir ? '—' : fmtSize(entry.size);

    const time = document.createElement('div');
    time.className = 'time';
    time.textContent = fmtTime(entry.mtime);

    row.appendChild(box);
    row.appendChild(nameCell);
    row.appendChild(size);
    row.appendChild(time);
    list.appendChild(row);
  });
  refreshToolbar();
}

function selectedNames() {
  return Array.from(state.selected);
}

function renderTrashList() {
  const list = el('list');
  list.className = 'list trash';
  list.textContent = '';
  const items = (state.trash && state.trash.items) || [];
  el('empty').textContent = '回收站为空';
  el('empty').hidden = items.length > 0;
  items.forEach((item) => {
    const row = document.createElement('div');
    row.className = 'row' + (item.isDir ? ' dir' : '') +
      (state.selected.has(item.id) ? ' selected' : '');

    const box = document.createElement('input');
    box.type = 'checkbox';
    box.checked = state.selected.has(item.id);
    box.onclick = (ev) => {
      ev.stopPropagation();
      if (box.checked) state.selected.add(item.id); else state.selected.delete(item.id);
      row.classList.toggle('selected', box.checked);
      refreshToolbar();
    };

    const nameCell = document.createElement('div');
    nameCell.className = 'name';
    const icon = document.createElement('span');
    icon.className = 'icon';
    icon.innerHTML = item.isDir ? SVG_DIR : SVG_FILE;
    const label = document.createElement('span');
    label.textContent = item.name;
    label.title = item.name + '\n原位置：' + item.path;
    nameCell.appendChild(icon);
    nameCell.appendChild(label);

    const size = document.createElement('div');
    size.className = 'size';
    size.textContent = fmtSize(item.size || 0);
    if (item.partial) size.title = '文件数过多，此大小可能偏小';

    const time = document.createElement('div');
    time.className = 'time';
    time.textContent = fmtTime(item.deletedAt);
    time.title = '原位置：' + item.path;

    const by = document.createElement('div');
    by.className = 'by';
    by.textContent = item.deletedBy || (item.legacy ? '旧版本' : '—');
    by.title = item.deletedBy ? '删除者：' + item.deletedBy : '';

    row.appendChild(box);
    row.appendChild(nameCell);
    row.appendChild(size);
    row.appendChild(time);
    row.appendChild(by);
    list.appendChild(row);
  });
  refreshToolbar();
}

function setTip(node, text) {
  node.dataset.tip = text;
  node.setAttribute('aria-label', text);
}

function refreshToolbar() {
  const trash = isTrashPath(state.path);
  const one = state.selected.size === 1;
  const any = state.selected.size > 0;
  const modify = can('M');
  el('btnNewFolder').hidden = trash || !modify;
  el('btnNewFile').hidden = trash || !modify;
  el('btnUpload').hidden = trash || !can('U');
  el('btnUploadDir').hidden = trash || !can('U');
  el('btnCopy').hidden = trash || !modify || !any;
  el('btnPaste').hidden = trash || !modify || !state.clip;
  setTip(el('btnPaste'), state.clip && state.clip.mode === 'copy' ? '粘贴副本' : '粘贴');
  el('btnRename').hidden = trash || !modify || !one;
  el('btnDelete').hidden = trash || !modify || !any;
  el('btnDownload').hidden = trash || !can('R') || !one;
  el('btnTrash').hidden = !modify || !trashOn();
  setTip(el('btnTrash'), trash ? '返回文件' : '回收站');
  setHidden(el('iconTrashBin'), trash);
  setHidden(el('iconTrashBack'), !trash);
  el('btnRestore').hidden = !trash || !any;
  el('btnPurge').hidden = !trash || !any;
  el('btnEmptyTrash').hidden = !trash || !(state.trash && state.trash.total);
  el('btnUp').disabled = trash || state.path === '/';
  document.body.classList.toggle('trash-mode', trash);
}

// ------------------------------------------------------------------- actions

async function newFolder() {
  const name = prompt('新文件夹名称');
  if (!name) return;
  try {
    await post('/mkdir', { path: state.path, name });
    await load();
  } catch (e) { toast('创建失败：' + e.message); }
}

const NEWFILE_ERRORS = {
  bad_name: '名称无效：不能为空，也不能包含 / 或 \\',
  exists: '同名文件已存在',
  forbidden: '没有新建文件的权限',
  not_found: '当前目录不存在',
};

async function newFile() {
  if (isTrashPath(state.path)) return;
  const raw = prompt('新文件名（含扩展名，例如 config.json）');
  if (raw === null) return;
  const name = raw.trim();
  if (!name) return;
  let created;
  try {
    created = await post('/newfile', { path: state.path, name });
  } catch (e) {
    toast('新建文件失败：' + (NEWFILE_ERRORS[e.data && e.data.error] || e.message));
    return;
  }
  await load();
  const entry = { name: created.name || name, isDir: false, size: 0 };
  const content = el('modalContent');
  content.textContent = '';
  state.editorDirty = false;
  try {
    const info = await api('/read?path=' + encodeURIComponent(joinPath(state.path, entry.name)));
    el('modal').hidden = false;
    openEditor(entry, content, info);
  } catch (e) {
    // Not previewable as text or no read permission: the file still exists.
    toast('已创建 ' + entry.name);
  }
}

async function renameSelected() {
  const from = selectedNames()[0];
  if (!from || isTrashPath(state.path)) return;
  const target = prompt('新名称', from);
  if (!target || target === from) return;
  try {
    await post('/rename', {
      from: joinPath(state.path, from),
      to: joinPath(state.path, target),
    });
    state.selected.clear();
    await load();
  } catch (e) { toast('重命名失败：' + e.message); }
}

async function deleteSelected() {
  const names = selectedNames();
  if (!names.length || isTrashPath(state.path)) return;
  const soft = trashOn();
  const question = soft
    ? '删除 ' + names.length + ' 项到回收站？'
    : '永久删除 ' + names.length + ' 项？此操作无法撤销，文件将被立即从磁盘移除。';
  if (!confirm(question)) return;
  try {
    const data = await post('/delete', { paths: names.map((n) => joinPath(state.path, n)) });
    const permanent = data && typeof data.permanent === 'boolean' ? data.permanent : !soft;
    toast(permanent ? '已永久删除 ' + names.length + ' 项' : '已移入回收站');
    state.selected.clear();
    await load();
  } catch (e) { toast('删除失败：' + e.message); }
}

// --------------------------------------------------------------- recycle bin

function toggleTrash() {
  if (!trashOn()) return;
  if (isTrashPath(state.path)) setPath('/');
  else setPath(TRASH_PATH);
}

function restoreSelected() {
  const ids = selectedNames();
  if (!ids.length) return;
  doRestore(ids, false);
}

async function doRestore(ids, overwrite) {
  let res = null;
  try {
    res = await post('/trash/restore', { ids, overwrite });
  } catch (e) {
    if (e.status === 409 && e.data) {
      res = e.data;
    } else {
      toast('恢复失败：' + e.message);
      return;
    }
  }
  const restored = res.restored || 0;
  const failed = res.failed || [];
  if (restored) toast('已恢复 ' + restored + ' 项');
  if (failed.length) {
    const conflicts = failed.filter((f) => f.code === 'exists').map((f) => f.id);
    if (conflicts.length) {
      const message = conflicts.length + ' 项在原位置已存在，是否覆盖？' +
        '\n被替换的内容会再次移入回收站。';
      if (confirm(message)) return doRestore(conflicts, true);
    } else {
      toast('恢复失败：' + failed[0].message);
    }
  }
  state.selected.clear();
  await load();
}

async function purgeSelected() {
  const ids = selectedNames();
  if (!ids.length) return;
  if (!confirm('彻底删除 ' + ids.length + ' 项？此操作不可恢复。')) return;
  try {
    const res = await post('/trash/purge', { ids });
    toast('已彻底删除 ' + (res.purged || 0) + ' 项');
    state.selected.clear();
    await load();
  } catch (e) { toast('删除失败：' + e.message); }
}

async function emptyTrash() {
  const total = state.trash ? state.trash.total : 0;
  if (!total) { toast('回收站是空的'); return; }
  if (!confirm('清空回收站中的 ' + total + ' 项？此操作不可恢复。')) return;
  try {
    const res = await post('/trash/empty', {});
    toast('已清空 ' + (res.purged || 0) + ' 项');
    state.selected.clear();
    await load();
  } catch (e) { toast('清空失败：' + e.message); }
}

function cutSelected() {
  const names = selectedNames();
  if (!names.length || isTrashPath(state.path)) return;
  state.clip = { mode: 'cut', dir: state.path, names };
  toast('已剪切 ' + names.length + ' 项');
  refreshToolbar();
}

function copySelected() {
  const names = selectedNames();
  if (!names.length || isTrashPath(state.path)) return;
  state.clip = { mode: 'copy', dir: state.path, names };
  toast('已复制 ' + names.length + ' 项');
  refreshToolbar();
}

const COPY_ERRORS = {
  copy_into_self: '不能把文件夹复制到它自身或子文件夹中',
  forbidden: '没有修改权限',
  not_found: '源文件已不存在',
  not_directory: '目标不是文件夹',
  bad_path: '路径不合法',
};

async function paste() {
  if (!state.clip || isTrashPath(state.path)) return;
  const clip = state.clip;
  const { mode, dir, names } = clip;
  if (mode === 'copy') {
    // Copying is allowed into the source directory itself: every copy gets a
    // "- 副本" suffix so it never collides with the original.
    const paths = names.map((name) => joinPath(dir, name));
    // Drop the clip first so the button hides at once and a second click
    // cannot queue a duplicate request; restore it only if the copy failed.
    state.clip = null;
    refreshToolbar();
    let copied = 0;
    try {
      const data = await post('/copy', { paths, dest: state.path });
      copied = data.copied || 0;
    } catch (e) {
      state.clip = clip;
      refreshToolbar();
      const code = e.data && e.data.error;
      toast('复制失败：' + (COPY_ERRORS[code] || e.message));
      return;
    }
    if (copied) toast('已复制 ' + copied + ' 项');
    await load();
    return;
  }
  if (dir === state.path) { toast('已在同一目录'); return; }
  let moved = 0;
  for (const name of names) {
    const target = joinPath(state.path, name);
    try {
      await post('/rename', { from: joinPath(dir, name), to: target });
      moved++;
    } catch (e) {
      toast('移动 ' + name + ' 失败：' + e.message);
    }
  }
  state.clip = null;
  state.selected.clear();
  if (moved) toast('已移动 ' + moved + ' 项');
  await load();
}

function downloadSelected() {
  const name = selectedNames()[0];
  if (!name) return;
  startDownload(name);
}

function isDirEntry(name) {
  const entry = state.entries.find((e) => e.name === name);
  return !!(entry && entry.isDir);
}

function startDownload(name) {
  const zip = isDirEntry(name);
  const url = '/files' + encodePath(joinPath(state.path, name)) + (zip ? '?zip=1' : '');
  const a = document.createElement('a');
  a.href = url;
  a.download = zip ? name + '.zip' : name;
  document.body.appendChild(a);
  a.click();
  a.remove();
}

function preview(entry) {
  const url = '/files' + encodePath(joinPath(state.path, entry.name));
  const content = el('modalContent');
  content.textContent = '';
  state.editorDirty = false;
  const ext = (entry.name.split('.').pop() || '').toLowerCase();
  const images = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'svg', 'avif'];
  const videos = ['mp4', 'webm', 'mkv', 'mov', 'avi'];
  const audios = ['mp3', 'wav', 'flac', 'ogg', 'm4a', 'aac', 'opus'];
  if (images.indexOf(ext) !== -1) {
    const img = document.createElement('img');
    img.src = url;
    img.alt = entry.name;
    content.appendChild(img);
  } else if (videos.indexOf(ext) !== -1) {
    const v = document.createElement('video');
    v.src = url;
    v.controls = true;
    if (can('T')) v.autoplay = false;
    content.appendChild(v);
  } else if (audios.indexOf(ext) !== -1) {
    const a = document.createElement('audio');
    a.src = url;
    a.controls = true;
    content.appendChild(a);
  } else if (entry.size <= TEXT_LIMIT) {
    openTextViewer(entry, content);
  } else {
    downloadSelectedByName(entry.name);
    return;
  }
  el('modal').hidden = false;
}

async function openTextViewer(entry, content, info) {
  const pre = document.createElement('pre');
  pre.textContent = '加载中…';
  content.appendChild(pre);
  if (!info) {
    try {
      info = await api('/read?path=' + encodeURIComponent(joinPath(state.path, entry.name)));
    } catch (e) {
      pre.textContent = e.status === 415
        ? '该文件不是 UTF-8 文本，无法在线预览。'
        : '无法预览：' + e.message;
      const download = document.createElement('button');
      download.textContent = '下载';
      download.onclick = () => downloadSelectedByName(entry.name);
      content.appendChild(download);
      return;
    }
  }
  pre.textContent = info.content;
  if (!info.canEdit || !can('M')) return;

  const bar = document.createElement('div');
  bar.className = 'preview-bar';
  const name = document.createElement('span');
  name.className = 'preview-name';
  name.textContent = entry.name;
  const edit = document.createElement('button');
  edit.className = 'primary';
  edit.textContent = '编辑';
  edit.onclick = () => openEditor(entry, content, info);
  bar.appendChild(name);
  bar.appendChild(edit);
  content.insertBefore(bar, pre);
}

function openEditor(entry, content, info) {
  const rel = joinPath(state.path, entry.name);
  // Textarea values always use LF, so remember the original line endings.
  const eol = info.content.indexOf('\r\n') === -1 ? 'lf' : 'crlf';
  content.textContent = '';

  const wrap = document.createElement('div');
  wrap.className = 'editor';
  const bar = document.createElement('div');
  bar.className = 'editor-bar';
  const name = document.createElement('span');
  name.className = 'preview-name';
  name.textContent = entry.name + ' — 编辑';
  const hint = document.createElement('span');
  hint.className = 'editor-hint';
  hint.textContent = 'Ctrl+S 保存';
  const save = document.createElement('button');
  save.className = 'primary';
  save.textContent = '保存';
  save.disabled = true;
  const cancel = document.createElement('button');
  cancel.textContent = '取消';
  bar.appendChild(name);
  bar.appendChild(hint);
  bar.appendChild(save);
  bar.appendChild(cancel);

  const area = document.createElement('textarea');
  area.spellcheck = false;
  area.value = info.content;
  const error = document.createElement('p');
  error.className = 'error';
  error.hidden = true;
  wrap.appendChild(bar);
  wrap.appendChild(area);
  wrap.appendChild(error);
  content.appendChild(wrap);

  const setDirty = (value) => {
    state.editorDirty = value;
    save.disabled = !value;
  };
  area.addEventListener('input', () => { error.hidden = true; setDirty(true); });
  area.addEventListener('keydown', (ev) => {
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === 's') {
      ev.preventDefault();
      if (!save.disabled) save.click();
    }
  });

  cancel.onclick = () => {
    if (state.editorDirty && !confirm('放弃未保存的修改？')) return;
    state.editorDirty = false;
    content.textContent = '';
    openTextViewer(entry, content, info);
  };

  save.onclick = async () => {
    let text = area.value.replace(/\r\n/g, '\n');
    if (eol === 'crlf') text = text.replace(/\n/g, '\r\n');
    let force = false;
    save.disabled = true;
    save.textContent = '保存中…';
    error.hidden = true;
    for (;;) {
      try {
        const res = await api('/write', {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            path: rel,
            content: text,
            mtimeMs: info.mtimeMs,
            bom: !!info.bom,
            force,
          }),
        });
        info.content = text;
        info.mtimeMs = res.mtimeMs;
        info.size = res.size;
        setDirty(false);
        save.textContent = '保存';
        toast('已保存');
        await load();
        return;
      } catch (e) {
        if (e.status === 409 && !force) {
          if (confirm('该文件已在服务器上被修改，仍要覆盖吗？')) {
            force = true;
            continue;
          }
          save.textContent = '保存';
          setDirty(true);
          return;
        }
        save.textContent = '保存';
        setDirty(true);
        error.textContent = '保存失败：' + e.message;
        error.hidden = false;
        return;
      }
    }
  };

  area.focus();
}

function closeModal() {
  if (state.editorDirty && !confirm('有未保存的修改，确定关闭？')) return;
  state.editorDirty = false;
  el('modal').hidden = true;
  el('modalContent').textContent = '';
}

function downloadSelectedByName(name) {
  startDownload(name);
}

// ------------------------------------------------------------------- uploads

function uploadRow(name) {
  const box = document.createElement('div');
  box.className = 'upload';
  const head = document.createElement('div');
  head.className = 'head';
  const label = document.createElement('span');
  label.className = 'name';
  label.textContent = name;
  label.title = name;
  const status = document.createElement('span');
  status.className = 'pct';
  status.textContent = '0%';
  const cancel = document.createElement('button');
  cancel.textContent = '×';
  cancel.title = '取消';
  head.appendChild(label);
  head.appendChild(status);
  head.appendChild(cancel);
  const bar = document.createElement('div');
  bar.className = 'bar';
  const fill = document.createElement('i');
  bar.appendChild(fill);
  const hint = document.createElement('div');
  hint.className = 'hint';
  hint.hidden = true;
  box.appendChild(head);
  box.appendChild(bar);
  box.appendChild(hint);
  el('uploads').appendChild(box);
  return {
    set(pct) {
      fill.style.width = Math.round(pct * 100) + '%';
      status.textContent = Math.round(pct * 100) + '%';
    },
    // Live progress line: refreshed after every chunk so it never shows the
    // stale amount captured when the upload started.
    progress(uploaded, total, speed, resumed) {
      const parts = [];
      if (resumed) parts.push('断点续传');
      parts.push(fmtSize(uploaded) + ' / ' + fmtSize(total));
      if (speed > 0) parts.push(fmtSize(speed) + '/s');
      hint.hidden = false;
      hint.textContent = parts.join(' · ');
      hint.title = hint.textContent;
    },
    done() {
      fill.style.width = '100%';
      status.textContent = '完成';
      hint.hidden = true;
      setTimeout(() => box.remove(), 1500);
    },
    fail(message, cancelled) {
      box.classList.add('failed');
      status.textContent = cancelled ? '已取消' : '失败';
      hint.hidden = false;
      hint.textContent = message;
      hint.title = message;
    },
    onCancel(fn) { cancel.onclick = fn; },
  };
}

// Sends one chunk. Returns the authoritative offset, or a conflict descriptor
// when the server already holds a different amount of data.
const UPLOAD_ERRORS = {
  not_found: '目标文件夹不存在',
  forbidden: '没有权限',
  bad_name: '文件名不合法',
  bad_path: '路径不合法',
  is_directory: '同名文件夹已存在',
  bad_offset: '分片偏移不正确',
  length_required: '请求缺少长度',
  incomplete_body: '连接中断',
  io_error: '服务器写入失败',
};

function errorTextOf(data, status) {
  if (data && UPLOAD_ERRORS[data.error]) return UPLOAD_ERRORS[data.error];
  if (data && data.message) return data.message;
  return 'HTTP ' + status;
}

async function uploadErrorText(res) {
  try {
    return errorTextOf(await res.json(), res.status);
  } catch (e) {
    // not JSON, fall through to the status line
  }
  return 'HTTP ' + res.status;
}

// Errors that retrying cannot fix; they abort the upload immediately.
function fatalError(text) {
  const err = new Error(text);
  err.fatal = true;
  return err;
}

async function putChunk(dir, name, blob, offset, total, restart, signal) {
  const url = API + '/upload?path=' + encodeURIComponent(dir) +
    '&name=' + encodeURIComponent(name) +
    '&offset=' + offset + '&total=' + total + (restart ? '&restart=1' : '');
  let wait = 0;
  for (;;) {
    try {
      const res = await fetch(url, {
        method: 'PUT',
        body: blob,
        credentials: 'same-origin',
        signal,
      });
      if (res.status === 409) {
        const info = await res.json().catch(() => ({}));
        // Only an offset mismatch is resumable; anything else (for instance a
        // folder holding the same name) must surface instead of being retried.
        if (info.error === 'offset_mismatch') {
          return { conflict: true, have: Number(info.offset) || 0 };
        }
        throw fatalError(errorTextOf(info, 409));
      }
      if (!res.ok) throw fatalError(await uploadErrorText(res));
      const data = await res.json();
      return { offset: Number(data.offset) || 0, done: !!data.done };
    } catch (e) {
      if (signal.aborted || e.name === 'AbortError' || e.fatal) throw e;
      if (++wait >= 3) throw e;
      await sleep(300 * wait);
    }
  }
}

async function uploadOne(file, name, base) {
  const total = file.size;
  const ui = uploadRow(name);
  const controller = new AbortController();
  const signal = controller.signal;
  ui.onCancel(() => controller.abort());
  // Snapshot the folder: browsing away mid-upload must not split the file.
  const dir = base || state.path;
  let offset = 0;
  let restart = false;
  let restarts = 0;
  let resumed = false;
  try {
    try {
      const probe = await api('/upload?path=' + encodeURIComponent(dir) +
        '&name=' + encodeURIComponent(name));
      const have = Number(probe && probe.offset) || 0;
      if (have > total) {
        restart = true;
      } else if (have > 0) {
        offset = have;
        resumed = have < total;
      }
    } catch (e) {
      if (signal.aborted) throw e;
    }
    const startedAt = Date.now();
    let sent = 0;
    if (total > 0) {
      ui.set(offset / total);
      ui.progress(offset, total, 0, resumed);
    }

    for (let guard = 0; guard < 100000; guard++) {
      const end = Math.min(offset + CHUNK, total);
      const blob = file.slice(offset, end);
      const res = await putChunk(dir, name, blob, offset, total, restart, signal);
      restart = false;
      if (res.conflict) {
        if (res.have <= total && res.have !== offset) {
          offset = res.have;
        } else if (restarts++ < 2) {
          offset = 0;
          restart = true;
          resumed = false;
        } else {
          throw new Error('服务器上的分片无法续传');
        }
        continue;
      }
      offset = res.offset;
      sent += blob.size;
      if (total > 0) {
        const seconds = Math.max((Date.now() - startedAt) / 1000, 0.001);
        ui.set(Math.min(offset / total, 1));
        ui.progress(offset, total, sent / seconds, resumed);
      }
      if (res.done) {
        ui.done();
        return;
      }
    }
    throw new Error('分片重试次数过多');
  } catch (e) {
    const cancelled = e.name === 'AbortError';
    ui.fail(cancelled ? '已取消' : e.message, cancelled);
    if (cancelled) {
      // Leave nothing behind for a cancelled upload.
      api('/upload?path=' + encodeURIComponent(dir) + '&name=' + encodeURIComponent(name),
        { method: 'DELETE' }).catch(() => {});
    }
    throw e;
  }
}

// `dataTransfer.files` flattens a dropped folder into a single 0 byte pseudo
// file, which used to be uploaded as an empty file. The entry API exposes the
// real hierarchy instead, so walk it before uploading.
function entryFile(entry) {
  return new Promise((resolve, reject) => entry.file(resolve, reject));
}

// `readEntries` returns at most one batch, so it must be called until empty.
function readAllEntries(reader) {
  return new Promise((resolve, reject) => {
    const out = [];
    const step = () => {
      reader.readEntries((batch) => {
        if (!batch.length) { resolve(out); return; }
        for (const entry of batch) out.push(entry);
        step();
      }, reject);
    };
    step();
  });
}

// Returns the number of files found so an empty folder can still be created.
async function collectEntry(entry, prefix, out) {
  if (entry.isFile) {
    try {
      const file = await entryFile(entry);
      out.push({ kind: 'file', file, name: prefix + entry.name });
      return 1;
    } catch (e) {
      return 0;
    }
  }
  if (!entry.isDirectory) return 0;
  const children = await readAllEntries(entry.createReader());
  let files = 0;
  for (const child of children) {
    files += await collectEntry(child, prefix + entry.name + '/', out);
  }
  if (!files) out.push({ kind: 'dir', name: prefix + entry.name });
  return files;
}

// Must read the entries synchronously during the drop event; only the batches
// afterwards may be awaited.
function droppedItems(dt) {
  const out = [];
  const roots = [];
  if (dt.items && dt.items.length) {
    for (const item of dt.items) {
      if (typeof item.webkitGetAsEntry !== 'function') continue;
      if (item.kind !== 'file') continue;
      const entry = item.webkitGetAsEntry();
      if (entry) roots.push(entry);
    }
  }
  if (roots.some((entry) => entry.isDirectory)) {
    return (async () => {
      for (const entry of roots) await collectEntry(entry, '', out);
      return out;
    })();
  }
  const files = dt.files ? Array.from(dt.files) : [];
  for (const file of files) {
    out.push({ kind: 'file', file, name: file.name });
  }
  return Promise.resolve(out);
}

// Best effort: uploads create their parent folders, but a folder that holds no
// files exists only if it is made explicitly, one segment at a time. `known`
// remembers the segments already handled so one drop does not re-create them.
async function ensureDir(rel, base, known) {
  if (!can('M')) return;
  let dir = base;
  for (const part of rel.split('/')) {
    if (!part) continue;
    dir = joinPath(dir, part);
    if (known.has(dir)) continue;
    try {
      await post('/mkdir', { path: parentPath(dir), name: part });
      known.add(dir);
    } catch (e) {
      // 409 means the folder is already there, which is fine.
      if (e.status !== 409) return;
      known.add(dir);
    }
  }
}

async function uploadFiles(items) {
  if (!can('U')) { toast('没有上传权限'); return; }
  if (isTrashPath(state.path)) { toast('回收站中无法上传'); return; }
  const list = Array.from(items).map((item) => (item.kind ? item : {
    kind: 'file',
    file: item,
    name: item.webkitRelativePath || item.name,
  }));
  if (!list.length) return;
  const base = state.path;
  const known = new Set();
  for (const item of list) {
    if (item.kind === 'dir') {
      await ensureDir(item.name, base, known);
      continue;
    }
    try {
      await uploadOne(item.file, item.name, base);
    } catch (e) {
      if (e.name !== 'AbortError') toast('上传失败：' + item.name);
    }
  }
  await load();
}

// ---------------------------------------------------------------- bootstrap

async function refreshSession() {
  try {
    state.session = await api('/session');
  } catch (e) {
    state.session = { perms: [], letters: '', user: '', loggedIn: false, trashEnabled: false };
  }
  applyBranding();
  renderWho();
  const allowed = state.session.perms.length > 0;
  el('browser').hidden = !allowed;
  // Only force the login form when guest browsing is disabled; otherwise the
  // topbar button opens it on demand.
  el('login').hidden = allowed;
  if (!allowed) {
    el('loginHint').textContent = '访客访问已关闭，请登录';
  }
  return allowed;
}

// `Fern.Title` renames the tab and top bar, `Fern.Notice` adds a banner.
function applyBranding() {
  const session = state.session || {};
  const title = session.title || 'fern';
  document.title = title;
  el('brand').textContent = title;
  const notice = el('notice');
  const text = session.notice || '';
  notice.textContent = text;
  notice.title = text;
  notice.hidden = !text;
}

function renderWho() {
  const who = el('who');
  who.textContent = '';
  const session = state.session || {};
  // Guest access disabled: the login form already covers the page, so the
  // "未登录 / 登录" pair would just repeat it.
  who.hidden = !session.loggedIn && !session.perms.length;
  if (session.loggedIn) {
    const name = document.createElement('span');
    name.textContent = session.user;
    const tag = document.createElement('span');
    tag.className = 'tag';
    tag.textContent = session.letters || '无权限';
    const out = document.createElement('button');
    out.textContent = '退出';
    out.onclick = async () => {
      try { await post('/logout'); } catch (e) { /* ignore */ }
      location.reload();
    };
    who.appendChild(name);
    who.appendChild(tag);
    who.appendChild(out);
  } else {
    const label = document.createElement('span');
    label.className = 'tag';
    label.textContent = session.letters ? '访客 ' + session.letters : '未登录';
    const login = document.createElement('button');
    login.textContent = '登录';
    login.onclick = () => { el('login').hidden = false; el('user').focus(); };
    who.appendChild(label);
    who.appendChild(login);
  }
}

function bind() {
  // The toolbar is sticky so the action buttons stay reachable while a long
  // listing scrolls; the sentinel above it tells us when it has been pinned.
  const toolbar = el('toolbar');
  if ('IntersectionObserver' in window) {
    const probe = new IntersectionObserver(([entry]) => {
      toolbar.classList.toggle('stuck', !entry.isIntersecting);
    });
    probe.observe(el('toolbarSentinel'));
  }

  el('loginForm').onsubmit = async (ev) => {
    ev.preventDefault();
    const error = el('loginError');
    error.hidden = true;
    try {
      await post('/login', { user: el('user').value, pass: el('pass').value });
      el('pass').value = '';
      await refreshSession();
      if (state.session.loggedIn) {
        toast('登录成功');
        setPath(pathFromHash());
      }
    } catch (e) {
      error.textContent = e.status === 429 ? '尝试次数过多，请稍后再试' : '登录失败：' + e.message;
      error.hidden = false;
    }
  };

  el('btnUp').onclick = () => setPath(parentPath(state.path));
  el('btnRefresh').onclick = () => load();
  el('btnNewFolder').onclick = newFolder;
  el('btnNewFile').onclick = newFile;
  el('btnRename').onclick = renameSelected;
  el('btnDelete').onclick = deleteSelected;
  el('btnTrash').onclick = toggleTrash;
  el('btnRestore').onclick = restoreSelected;
  el('btnPurge').onclick = purgeSelected;
  el('btnEmptyTrash').onclick = emptyTrash;
  el('btnCopy').onclick = copySelected;
  el('btnPaste').onclick = paste;
  el('btnDownload').onclick = downloadSelected;
  el('btnUpload').onclick = () => el('fileInput').click();
  el('btnUploadDir').onclick = () => el('dirInput').click();
  el('fileInput').onchange = (ev) => { uploadFiles(ev.target.files); ev.target.value = ''; };
  el('dirInput').onchange = (ev) => { uploadFiles(ev.target.files); ev.target.value = ''; };
  el('modalClose').onclick = closeModal;
  el('modal').onclick = (ev) => { if (ev.target === el('modal')) closeModal(); };

  document.addEventListener('keydown', (ev) => {
    // Only text entry swallows the shortcuts: after ticking a row checkbox the
    // focus sits on an <input>, and copy/paste must still work there.
    const tag = ev.target.tagName;
    const typing = tag === 'TEXTAREA' ||
      ev.target.isContentEditable ||
      (tag === 'INPUT' && !['checkbox', 'radio', 'button', 'submit', 'file'].includes(ev.target.type));
    if (typing) return;
    const trash = isTrashPath(state.path);
    if (ev.key === 'F2' && !trash) { renameSelected(); ev.preventDefault(); }
    if (ev.key === 'Delete') {
      if (trash) purgeSelected(); else deleteSelected();
      ev.preventDefault();
    }
    if (ev.key === 'F5') { load(); ev.preventDefault(); }
    if (ev.key === 'Escape') { state.selected.clear(); renderList(); }
    if (!trash && (ev.ctrlKey || ev.metaKey) && ev.key === 'x') { cutSelected(); ev.preventDefault(); }
    if (!trash && (ev.ctrlKey || ev.metaKey) && ev.key === 'c' && state.selected.size) { copySelected(); ev.preventDefault(); }
    if (!trash && (ev.ctrlKey || ev.metaKey) && ev.key === 'v') { paste(); ev.preventDefault(); }
  });

  let depth = 0;
  window.addEventListener('dragenter', (ev) => {
    if (!can('U')) return;
    ev.preventDefault();
    depth++;
    app.classList.add('dragging');
  });
  window.addEventListener('dragover', (ev) => ev.preventDefault());
  window.addEventListener('dragleave', () => {
    depth = Math.max(0, depth - 1);
    if (depth === 0) app.classList.remove('dragging');
  });
  window.addEventListener('drop', (ev) => {
    ev.preventDefault();
    depth = 0;
    app.classList.remove('dragging');
    if (!ev.dataTransfer) return;
    // Read the entries now: the DataTransfer is emptied once the event returns.
    droppedItems(ev.dataTransfer).then((items) => {
      if (items.length) return uploadFiles(items);
    }, () => {});
  });

  document.addEventListener('paste', (ev) => {
    if (!ev.clipboardData || !ev.clipboardData.files.length) return;
    uploadFiles(ev.clipboardData.files);
  });

  window.addEventListener('hashchange', () => {
    const target = pathFromHash();
    if (target !== state.path) {
      state.path = target;
      // Breadcrumb links bypass `setPath`, so the old directory's selection
      // would otherwise survive and make toolbar actions target stale names.
      state.selected.clear();
      load();
    }
  });
}

async function boot() {
  state.path = pathFromHash();
  bind();
  const allowed = await refreshSession();
  if (allowed) {
    if (!state.session.loggedIn && state.session.perms.length === 0) return;
    await load();
  }
}

boot();
