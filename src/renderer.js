'use strict';

const state = {
  settings: null,
  library: null,
  currentView: 'all',
  currentCategory: null,
  search: '',
  sort: 'newest',
  pendingCategoryRenameName: null,
  pendingMoveHash: null,
  pointerDrag: null,
};

const els = {
  allTab: document.getElementById('all-tab'),
  allCount: document.getElementById('all-count'),
  unclassifiedTab: document.getElementById('unclassified-tab'),
  unclassifiedCount: document.getElementById('unclassified-count'),
  categoryList: document.getElementById('category-list'),
  addCategoryButton: document.getElementById('add-category-button'),
  sourceFolderList: document.getElementById('source-folder-list'),
  addSourceFolderButton: document.getElementById('add-source-folder-button'),
  rootFolderButton: document.getElementById('root-folder-button'),
  viewTitle: document.getElementById('view-title'),
  viewSubtitle: document.getElementById('view-subtitle'),
  imageGrid: document.getElementById('image-grid'),
  emptyState: document.getElementById('empty-state'),
  mainDropTarget: document.getElementById('main-drop-target'),
  searchInput: document.getElementById('search-input'),
  sortSelect: document.getElementById('sort-select'),
  refreshButton: document.getElementById('refresh-button'),
  openFolderButton: document.getElementById('open-folder-button'),
  settingsButton: document.getElementById('settings-button'),
  categoryDialog: document.getElementById('category-dialog'),
  categoryForm: document.getElementById('category-form'),
  categoryNameInput: document.getElementById('category-name-input'),
  cancelCategoryButton: document.getElementById('cancel-category-button'),
  categoryRenameDialog: document.getElementById('category-rename-dialog'),
  categoryRenameForm: document.getElementById('category-rename-form'),
  categoryRenameInput: document.getElementById('category-rename-input'),
  cancelCategoryRenameButton: document.getElementById('cancel-category-rename-button'),
  moveDialog: document.getElementById('move-dialog'),
  moveForm: document.getElementById('move-form'),
  moveFolderSelect: document.getElementById('move-folder-select'),
  moveNewFolderInput: document.getElementById('move-new-folder-input'),
  cancelMoveButton: document.getElementById('cancel-move-button'),
  settingsDialog: document.getElementById('settings-dialog'),
  settingsForm: document.getElementById('settings-form'),
  settingsRootFolder: document.getElementById('settings-root-folder'),
  settingsRootButton: document.getElementById('settings-root-button'),
  sourcePatternPreset: document.getElementById('source-pattern-preset'),
  sourcePatternRegex: document.getElementById('source-pattern-regex'),
  manualFolderList: document.getElementById('manual-folder-list'),
  tileSizeInput: document.getElementById('tile-size-input'),
  tileSizeValue: document.getElementById('tile-size-value'),
  darkModeInput: document.getElementById('dark-mode-input'),
  toast: document.getElementById('toast'),
};

function showToast(message) {
  els.toast.textContent = message;
  els.toast.classList.add('visible');
  clearTimeout(showToast.timer);
  showToast.timer = setTimeout(() => els.toast.classList.remove('visible'), 2400);
}

function errorText(error) {
  return error?.message || String(error);
}

function shortPath(path) {
  if (!path) return '';
  const normalized = path.replace(/\//g, '\\');
  const parts = normalized.split('\\').filter(Boolean);
  if (parts.length <= 3) return normalized;
  return `${parts[0]}\\...\\${parts.slice(-2).join('\\')}`;
}

function formatBytes(bytes) {
  if (!bytes) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 10 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
}

function formatDate(ms) {
  if (!ms) return 'Unknown date';
  return new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  }).format(new Date(ms));
}

function tileSize() {
  return Number(state.settings?.tileSize || 168);
}

function applyUiSettings() {
  const settings = state.settings;
  if (!settings) return;
  document.body.classList.toggle('light-mode', !settings.darkMode);
  document.documentElement.style.setProperty('--tile-size', `${tileSize()}px`);
}

function syncSettingsDialog() {
  const settings = state.settings;
  const library = state.library;
  if (!settings) return;
  els.settingsRootFolder.textContent = library?.root || settings.lastRoot || 'No folder chosen';
  els.settingsRootFolder.title = library?.root || settings.lastRoot || '';
  els.tileSizeInput.value = String(tileSize());
  els.tileSizeValue.textContent = `${tileSize()}px`;
  els.darkModeInput.checked = settings.darkMode;
  els.sourcePatternPreset.value = library?.sourcePatternPreset || '';
  els.sourcePatternRegex.value = library?.sourcePatternRegex || '';
  renderManualFolderList();
}

function renderManualFolderList() {
  const library = state.library;
  els.manualFolderList.innerHTML = '';
  const manualFolders = (library?.sourceFolders || []).filter(folder => folder.isManual);
  if (!manualFolders.length) {
    const empty = document.createElement('div');
    empty.className = 'manual-folder-empty';
    empty.textContent = 'No manually added folders yet.';
    els.manualFolderList.append(empty);
    return;
  }
  for (const folder of manualFolders) {
    const row = document.createElement('div');
    row.className = 'manual-folder-row';
    const label = document.createElement('span');
    label.textContent = folder.name;
    const removeButton = document.createElement('button');
    removeButton.type = 'button';
    removeButton.className = 'button compact secondary';
    removeButton.textContent = 'Remove';
    removeButton.addEventListener('click', () => removeManualSourceFolder(folder.name));
    row.append(label, removeButton);
    els.manualFolderList.append(row);
  }
}

function visibleImages() {
  const library = state.library;
  if (!library) return [];

  let images = library.images;
  if (state.currentView === 'unclassified') {
    images = images.filter(image => !image.category);
  } else if (state.currentView === 'category') {
    images = images.filter(image => image.category === state.currentCategory);
  }

  const query = state.search.trim().toLowerCase();
  if (query) {
    images = images.filter(image => `${image.name} ${image.relativePath}`.toLowerCase().includes(query));
  }

  images = [...images];
  images.sort((a, b) => {
    if (state.sort === 'name') {
      return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });
    }
    if (state.sort === 'size') {
      return b.size - a.size || a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });
    }
    return b.modifiedMs - a.modifiedMs || a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });
  });

  return images;
}

function renderSettings() {
  if (!state.settings) return;
  applyUiSettings();
  const library = state.library;
  els.rootFolderButton.textContent = library?.root ? shortPath(library.root) : 'Choose folder...';
  els.rootFolderButton.title = library?.root ? `Change root folder\n${library.root}` : 'Choose a root folder';
  syncSettingsDialog();
}

function renderSidebar() {
  const library = state.library;
  const allCount = library?.images.length || 0;
  const unclassifiedCount = library?.unclassifiedCount || 0;

  els.allCount.textContent = String(allCount);
  els.unclassifiedCount.textContent = String(unclassifiedCount);
  els.allTab.classList.toggle('active', state.currentView === 'all');
  els.unclassifiedTab.classList.toggle('active', state.currentView === 'unclassified');

  els.categoryList.innerHTML = '';
  const categories = library?.categories || [];
  if (!categories.length) {
    const empty = document.createElement('button');
    empty.className = 'category-empty';
    empty.type = 'button';
    empty.textContent = 'Add your first category';
    empty.addEventListener('click', openCategoryDialog);
    els.categoryList.append(empty);
  } else {
    for (const category of categories) {
      const row = document.createElement('div');
      row.className = 'category-row';
      row.dataset.categoryName = category.name;

      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'category-item';
      button.classList.toggle('active', state.currentView === 'category' && state.currentCategory === category.name);
      button.innerHTML = '<span class="category-name"></span><span class="count-pill"></span>';
      button.querySelector('.category-name').textContent = category.name;
      button.querySelector('.count-pill').textContent = String(category.count);
      button.addEventListener('click', () => selectCategory(category.name));

      const renameButton = document.createElement('button');
      renameButton.type = 'button';
      renameButton.className = 'category-rename-button';
      renameButton.title = `Rename ${category.name}`;
      renameButton.textContent = 'Rename';
      renameButton.addEventListener('click', event => {
        event.stopPropagation();
        openCategoryRenameDialog(category.name);
      });

      const deleteButton = document.createElement('button');
      deleteButton.type = 'button';
      deleteButton.className = 'category-rename-button';
      deleteButton.title = `Delete ${category.name}`;
      deleteButton.textContent = 'Delete';
      deleteButton.addEventListener('click', event => {
        event.stopPropagation();
        deleteCategoryConfirm(category.name);
      });

      row.append(button, renameButton, deleteButton);
      els.categoryList.append(row);
    }
  }

  els.sourceFolderList.innerHTML = '';
  const sourceFolders = library?.sourceFolders || [];
  if (!sourceFolders.length) {
    const empty = document.createElement('div');
    empty.className = 'category-empty';
    empty.textContent = 'No source folders detected yet.';
    els.sourceFolderList.append(empty);
  } else {
    for (const folder of sourceFolders) {
      const row = document.createElement('div');
      row.className = 'source-folder-row';
      row.innerHTML = '<span class="category-name"></span><span class="count-pill"></span>';
      row.querySelector('.category-name').textContent = folder.name;
      row.querySelector('.count-pill').textContent = String(folder.imageCount);
      els.sourceFolderList.append(row);
    }
  }
}

function renderHeader() {
  const library = state.library;
  if (state.currentView === 'all') {
    els.viewTitle.textContent = 'All Images';
  } else if (state.currentView === 'unclassified') {
    els.viewTitle.textContent = 'Unclassified';
  } else {
    els.viewTitle.textContent = state.currentCategory || 'Category';
  }
  els.viewSubtitle.textContent = library?.root || 'No root folder chosen yet';
}

function categoryOptionsHtml(selected) {
  const categories = state.library?.categories || [];
  let html = `<option value="">Unclassified</option>`;
  for (const category of categories) {
    const isSelected = category.name === selected ? 'selected' : '';
    html += `<option value="${category.name}" ${isSelected}>${category.name}</option>`;
  }
  return html;
}

function renderImages() {
  const images = visibleImages();
  els.imageGrid.innerHTML = '';
  els.emptyState.classList.toggle('visible', images.length === 0);

  if (!images.length) {
    if (!state.library?.root) {
      els.emptyState.innerHTML = '';
      const button = document.createElement('button');
      button.className = 'button';
      button.type = 'button';
      button.textContent = 'Choose Root Folder';
      button.addEventListener('click', changeRootFolder);
      els.emptyState.append('No root folder chosen yet. ', button);
    } else {
      els.emptyState.textContent = state.search
        ? 'No images match that search.'
        : 'No images found yet. Add a monthly folder or a manual source folder, then Rescan.';
    }
    return;
  }

  for (const image of images) {
    const card = document.createElement('article');
    card.className = 'image-card';
    card.dataset.hash = image.hash;
    card.dataset.path = image.path;
    card.dataset.name = image.name;
    card.innerHTML = `
      <div class="thumbnail">
        <img class="thumb-image" alt="" loading="lazy">
      </div>
      <div class="card-main">
        <div class="file-title"></div>
        <div class="file-meta"></div>
        <div class="card-controls">
          <select class="category-select"></select>
          <button class="button compact secondary move-button" type="button">Move</button>
        </div>
        <div class="card-actions">
          <button class="button compact ghost open-button" type="button">Open</button>
          <button class="button compact ghost reveal-button" type="button">Show</button>
        </div>
      </div>
    `;

    card.querySelector('.thumb-image').src = window.categorizerAPI.getFileUrl(image.path);
    card.querySelector('.file-title').textContent = image.name;
    const folderText = image.sourceFolder ? ` · ${image.sourceFolder}` : '';
    const badge = image.classifiedBy === 'auto' ? ' · auto' : image.classifiedBy === 'manual' ? ' · manual' : '';
    card.querySelector('.file-meta').textContent = `${formatDate(image.modifiedMs)} · ${formatBytes(image.size)}${folderText}${badge}`;

    const select = card.querySelector('.category-select');
    select.innerHTML = categoryOptionsHtml(image.category);
    select.addEventListener('change', () => assignCategory(image.hash, select.value || null));

    card.querySelector('.move-button').addEventListener('click', () => openMoveDialog(image));
    card.querySelector('.open-button').addEventListener('click', () => openImage(image.path));
    card.querySelector('.reveal-button').addEventListener('click', () => revealImage(image.path));
    card.addEventListener('pointerdown', event => startPointerDrag(event, card));

    els.imageGrid.append(card);
  }
}

function render() {
  renderSettings();
  renderSidebar();
  renderHeader();
  renderImages();
}

async function loadSettings() {
  state.settings = await window.categorizerAPI.getSettings();
}

async function refreshLibrary() {
  if (!state.settings?.lastRoot) {
    state.library = null;
    return;
  }
  try {
    state.library = await window.categorizerAPI.scanLibrary(state.settings.lastRoot);
  } catch (error) {
    showToast(errorText(error));
    state.library = null;
  }
}

async function refreshAll() {
  await loadSettings();
  await refreshLibrary();
  if (state.currentView === 'category' && !(state.library?.categories || []).some(c => c.name === state.currentCategory)) {
    state.currentView = 'all';
    state.currentCategory = null;
  }
  render();
}

function selectAll() {
  cancelPointerDrag();
  state.currentView = 'all';
  state.currentCategory = null;
  render();
}

function selectUnclassified() {
  cancelPointerDrag();
  state.currentView = 'unclassified';
  state.currentCategory = null;
  render();
}

function selectCategory(name) {
  cancelPointerDrag();
  state.currentView = 'category';
  state.currentCategory = name;
  render();
}

async function assignCategory(hash, category) {
  if (!state.library) return;
  try {
    state.library = await window.categorizerAPI.assignCategory(state.library.root, hash, category);
    render();
    showToast(category ? `Assigned to ${category}` : 'Marked unclassified');
  } catch (error) {
    showToast(errorText(error));
  }
}

async function openImage(filePath) {
  try {
    await window.categorizerAPI.openImage(filePath);
  } catch (error) {
    showToast(errorText(error));
  }
}

async function revealImage(filePath) {
  try {
    await window.categorizerAPI.revealImage(filePath);
  } catch (error) {
    showToast(errorText(error));
  }
}

function openCategoryDialog() {
  els.categoryNameInput.value = '';
  els.categoryDialog.showModal();
  setTimeout(() => els.categoryNameInput.focus(), 0);
}

function closeCategoryDialog() {
  els.categoryDialog.close();
}

function openCategoryRenameDialog(name) {
  state.pendingCategoryRenameName = name;
  els.categoryRenameInput.value = name;
  els.categoryRenameDialog.showModal();
  setTimeout(() => {
    els.categoryRenameInput.focus();
    els.categoryRenameInput.select();
  }, 0);
}

function closeCategoryRenameDialog() {
  state.pendingCategoryRenameName = null;
  els.categoryRenameDialog.close();
}

function openMoveDialog(image) {
  state.pendingMoveHash = image.hash;
  const folders = state.library?.sourceFolders || [];
  els.moveFolderSelect.innerHTML = folders
    .map(folder => `<option value="${folder.name}" ${folder.name === image.sourceFolder ? 'selected' : ''}>${folder.name}</option>`)
    .join('');
  els.moveNewFolderInput.value = '';
  els.moveDialog.showModal();
}

function closeMoveDialog() {
  state.pendingMoveHash = null;
  els.moveDialog.close();
}

async function submitMove() {
  if (!state.pendingMoveHash || !state.library) return;
  const targetFolder = els.moveNewFolderInput.value.trim() || els.moveFolderSelect.value;
  if (!targetFolder) return;
  try {
    state.library = await window.categorizerAPI.moveImage(state.library.root, state.pendingMoveHash, targetFolder);
    closeMoveDialog();
    render();
    showToast(`Moved to ${targetFolder}`);
  } catch (error) {
    showToast(errorText(error));
  }
}

function openSettingsDialog() {
  syncSettingsDialog();
  els.settingsDialog.showModal();
}

function closeSettingsDialog() {
  els.settingsDialog.close();
}

async function createCategory(name) {
  if (!state.library) {
    showToast('Choose a root folder first.');
    return;
  }
  try {
    state.library = await window.categorizerAPI.createCategory(state.library.root, name);
    closeCategoryDialog();
    render();
    showToast(`Created ${name.trim()}`);
  } catch (error) {
    showToast(errorText(error));
  }
}

async function renamePendingCategory(newName) {
  if (!state.pendingCategoryRenameName || !state.library) return;
  const oldName = state.pendingCategoryRenameName;
  try {
    state.library = await window.categorizerAPI.renameCategory(state.library.root, oldName, newName);
    const wasCurrent = state.currentCategory === oldName;
    closeCategoryRenameDialog();
    if (wasCurrent) state.currentCategory = newName.trim();
    render();
    showToast(`Renamed to ${newName.trim()}`);
  } catch (error) {
    showToast(errorText(error));
  }
}

async function deleteCategoryConfirm(name) {
  if (!state.library) return;
  if (!window.confirm(`Delete category "${name}"? Images in it become unclassified.`)) return;
  try {
    state.library = await window.categorizerAPI.deleteCategory(state.library.root, name);
    if (state.currentCategory === name) {
      state.currentView = 'all';
      state.currentCategory = null;
    }
    render();
    showToast(`Deleted ${name}`);
  } catch (error) {
    showToast(errorText(error));
  }
}

async function removeManualSourceFolder(name) {
  if (!state.library) return;
  try {
    state.library = await window.categorizerAPI.removeManualSourceFolder(state.library.root, name);
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

async function addManualSourceFolder() {
  if (!state.library) {
    showToast('Choose a root folder first.');
    return;
  }
  try {
    const library = await window.categorizerAPI.chooseManualSourceFolder(state.library.root);
    if (!library) return;
    state.library = library;
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

async function saveSourcePattern() {
  if (!state.library) return;
  const preset = els.sourcePatternPreset.value || null;
  const regex = els.sourcePatternRegex.value.trim() || null;
  try {
    state.library = await window.categorizerAPI.setSourcePattern(state.library.root, preset, regex);
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

async function saveUiSettingsNow() {
  if (!state.settings) return;
  const tileSizeValue = Number(els.tileSizeInput.value);
  const darkMode = els.darkModeInput.checked;
  try {
    state.settings = await window.categorizerAPI.setTileSize(tileSizeValue);
    state.settings = await window.categorizerAPI.setDarkMode(darkMode);
    renderSettings();
  } catch (error) {
    showToast(errorText(error));
  }
}

function applyPendingUiSettings() {
  if (!state.settings) return;
  state.settings.tileSize = Number(els.tileSizeInput.value);
  state.settings.darkMode = els.darkModeInput.checked;
  applyUiSettings();
}

async function changeRootFolder() {
  try {
    const library = await window.categorizerAPI.chooseRootFolder(state.library?.root);
    if (!library) return;
    state.library = library;
    state.settings = await window.categorizerAPI.getSettings();
    state.currentView = 'all';
    state.currentCategory = null;
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

async function openCurrentRootFolder() {
  if (!state.library?.root) return;
  try {
    await window.categorizerAPI.openRootFolder(state.library.root);
  } catch (error) {
    showToast(errorText(error));
  }
}

function clearPointerDropTargets() {
  document.querySelectorAll('.pointer-drop-over').forEach(element => element.classList.remove('pointer-drop-over'));
}

function categoryDropTargetFromPoint(x, y) {
  const element = document.elementFromPoint(x, y);
  const categoryRow = element?.closest?.('.category-row');
  if (categoryRow?.dataset.categoryName) {
    return { element: categoryRow, category: categoryRow.dataset.categoryName };
  }
  if (element?.closest?.('#unclassified-tab')) {
    return { element: els.unclassifiedTab, category: null };
  }
  return null;
}

function startPointerDrag(event, card) {
  if (event.button !== 0 || event.target.closest('button, select')) return;

  state.pointerDrag = {
    card,
    hash: card.dataset.hash,
    name: card.dataset.name || card.dataset.hash,
    startX: event.clientX,
    startY: event.clientY,
    x: event.clientX,
    y: event.clientY,
    active: false,
    ghost: null,
    dropTarget: null,
  };

  card.setPointerCapture?.(event.pointerId);
  card.addEventListener('pointermove', onPointerDragMove);
  card.addEventListener('pointerup', onPointerDragEnd, { once: true });
  card.addEventListener('pointercancel', cancelPointerDrag, { once: true });
}

function activatePointerDrag(drag) {
  drag.active = true;
  drag.card.classList.add('dragging');
  document.body.classList.add('pointer-dragging');

  const ghost = document.createElement('div');
  ghost.className = 'drag-ghost';
  ghost.textContent = drag.name;
  document.body.append(ghost);
  drag.ghost = ghost;
  moveDragGhost(drag);
}

function moveDragGhost(drag) {
  if (!drag.ghost) return;
  drag.ghost.style.transform = `translate(${drag.x + 12}px, ${drag.y + 12}px)`;
}

function onPointerDragMove(event) {
  const drag = state.pointerDrag;
  if (!drag) return;

  drag.x = event.clientX;
  drag.y = event.clientY;

  if (!drag.active) {
    const dx = drag.x - drag.startX;
    const dy = drag.y - drag.startY;
    if (Math.hypot(dx, dy) < 6) return;
    activatePointerDrag(drag);
  }

  event.preventDefault();
  moveDragGhost(drag);
  clearPointerDropTargets();
  drag.dropTarget = categoryDropTargetFromPoint(drag.x, drag.y);
  drag.dropTarget?.element.classList.add('pointer-drop-over');
}

function finishPointerDrag() {
  const drag = state.pointerDrag;
  if (!drag) return null;

  drag.card.removeEventListener('pointermove', onPointerDragMove);
  drag.card.classList.remove('dragging');
  document.body.classList.remove('pointer-dragging');
  clearPointerDropTargets();
  drag.ghost?.remove();
  state.pointerDrag = null;
  return drag;
}

function cancelPointerDrag() {
  finishPointerDrag();
}

function onPointerDragEnd(event) {
  const drag = finishPointerDrag();
  if (!drag?.active) return;
  event.preventDefault();
  if (drag.dropTarget) {
    assignCategory(drag.hash, drag.dropTarget.category);
  }
}

function installEvents() {
  els.allTab.addEventListener('click', selectAll);
  els.unclassifiedTab.addEventListener('click', selectUnclassified);
  els.addCategoryButton.addEventListener('click', openCategoryDialog);
  els.addSourceFolderButton.addEventListener('click', addManualSourceFolder);
  els.rootFolderButton.addEventListener('click', changeRootFolder);
  els.settingsButton.addEventListener('click', openSettingsDialog);
  els.refreshButton.addEventListener('click', refreshAll);
  els.openFolderButton.addEventListener('click', openCurrentRootFolder);
  els.searchInput.addEventListener('input', () => {
    state.search = els.searchInput.value;
    renderImages();
  });
  els.sortSelect.addEventListener('change', () => {
    state.sort = els.sortSelect.value;
    renderImages();
  });

  els.categoryForm.addEventListener('submit', event => {
    event.preventDefault();
    createCategory(els.categoryNameInput.value);
  });
  els.cancelCategoryButton.addEventListener('click', closeCategoryDialog);

  els.categoryRenameForm.addEventListener('submit', event => {
    event.preventDefault();
    renamePendingCategory(els.categoryRenameInput.value);
  });
  els.cancelCategoryRenameButton.addEventListener('click', closeCategoryRenameDialog);

  els.moveForm.addEventListener('submit', event => {
    event.preventDefault();
    submitMove();
  });
  els.cancelMoveButton.addEventListener('click', closeMoveDialog);

  els.settingsForm.addEventListener('submit', event => {
    event.preventDefault();
    saveUiSettingsNow();
    closeSettingsDialog();
  });
  els.settingsRootButton.addEventListener('click', changeRootFolder);
  els.sourcePatternPreset.addEventListener('change', () => {
    const preset = els.sourcePatternPreset.value;
    const presetRegexMap = {
      'YYYY-MM': '^\\d{4}-\\d{2}$',
      'YYYY_MM': '^\\d{4}_\\d{2}$',
      'MM-YYYY': '^\\d{2}-\\d{4}$',
      'Month YYYY': '^(?i)(January|February|March|April|May|June|July|August|September|October|November|December) \\d{4}$',
    };
    if (presetRegexMap[preset]) {
      els.sourcePatternRegex.value = presetRegexMap[preset];
    }
    saveSourcePattern();
  });
  els.sourcePatternRegex.addEventListener('change', saveSourcePattern);
  els.tileSizeInput.addEventListener('input', applyPendingUiSettings);
  els.darkModeInput.addEventListener('change', applyPendingUiSettings);

  document.addEventListener('keydown', event => {
    if (event.key === 'Escape') {
      if (els.categoryDialog.open) closeCategoryDialog();
      if (els.categoryRenameDialog.open) closeCategoryRenameDialog();
      if (els.moveDialog.open) closeMoveDialog();
      if (els.settingsDialog.open) closeSettingsDialog();
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'r') {
      event.preventDefault();
      refreshAll();
    }
  });
}

async function init() {
  installEvents();
  await refreshAll();
}

init();
