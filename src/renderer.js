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
  virtualImages: null,
  virtualStart: 0,
  virtualEnd: 0,
  cardHeight: null,
  scrollFrameRequested: false,
  // Analysis state
  analyzing: false,
  analysisQueue: [],   // [{type: 'text'|'nsfw', force: bool}]
  analysisRunning: null, // 'text' | 'nsfw' | null
  autoRefresh: null,
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
  rootFolderSelect: document.getElementById('root-folder-select'),
  viewTitle: document.getElementById('view-title'),
  viewSubtitle: document.getElementById('view-subtitle'),
  imageGrid: document.getElementById('image-grid'),
  emptyState: document.getElementById('empty-state'),
  mainDropTarget: document.getElementById('main-drop-target'),
  searchInput: document.getElementById('search-input'),
  statusMessage: document.getElementById('status-message'),
  sortSelect: document.getElementById('sort-select'),
  refreshButton: document.getElementById('refresh-button'),
  analyzeButton: document.getElementById('analyze-button'),
  reanalyzeButton: document.getElementById('reanalyze-button'),
  cancelAnalysisButton: document.getElementById('cancel-analysis-button'),
  analyzeTextCheck: document.getElementById('analyze-text-check'),
  analyzeNsfwCheck: document.getElementById('analyze-nsfw-check'),
  extractTextCheck: document.getElementById('extract-text-check'),
  analyzeNsfwCheckLabel: document.getElementById('analyze-nsfw-check-label'),
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
  ocrWordThresholdInput: document.getElementById('ocr-word-threshold-input'),
  ocrWordThresholdValue: document.getElementById('ocr-word-threshold-value'),
  ocrAreaThresholdInput: document.getElementById('ocr-area-threshold-input'),
  ocrAreaThresholdValue: document.getElementById('ocr-area-threshold-value'),
  nsfwThresholdInput: document.getElementById('nsfw-threshold-input'),
  nsfwThresholdValue: document.getElementById('nsfw-threshold-value'),
  nsfwModelHint: document.getElementById('nsfw-model-hint'),
  downloadNsfwModelButton: document.getElementById('download-nsfw-model-button'),
  nsfwModelReport: document.getElementById('nsfw-model-report'),
  autoRefreshEnabledInput: document.getElementById('auto-refresh-enabled-input'),
  autoRefreshOptions: document.getElementById('auto-refresh-options'),
  autoRefreshTimeInput: document.getElementById('auto-refresh-time-input'),
  autoRefreshRootList: document.getElementById('auto-refresh-root-list'),
  autoRefreshNsfwInput: document.getElementById('auto-refresh-nsfw-input'),
  autoRefreshTextAnalysisInput: document.getElementById('auto-refresh-text-analysis-input'),
  autoRefreshTextExtractionInput: document.getElementById('auto-refresh-text-extraction-input'),
  autoRefreshLowPriorityInput: document.getElementById('auto-refresh-low-priority-input'),
  autoRefreshToastInput: document.getElementById('auto-refresh-toast-input'),
  autoRefreshStatus: document.getElementById('auto-refresh-status'),
  toast: document.getElementById('toast'),
};

function showToast(message) {
  els.toast.textContent = message;
  els.toast.classList.add('visible');
  clearTimeout(showToast.timer);
  showToast.timer = setTimeout(() => els.toast.classList.remove('visible'), 2400);
}

function setStatus(message) {
  els.statusMessage.textContent = message || '';
  els.statusMessage.title = message || '';
  clearTimeout(setStatus.timer);
  if (message) {
    setStatus.timer = setTimeout(() => {
      els.statusMessage.textContent = '';
      els.statusMessage.title = '';
    }, 5000);
  }
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

function imageCountLabel(count) {
  return `${count} image${count === 1 ? '' : 's'}`;
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
  const wordThreshold = library?.ocrWordThreshold ?? 35;
  const areaThresholdPercent = Math.round((library?.ocrAreaThreshold ?? 0.05) * 100);
  els.ocrWordThresholdInput.value = String(wordThreshold);
  els.ocrWordThresholdValue.textContent = `${wordThreshold} words`;
  els.ocrAreaThresholdInput.value = String(areaThresholdPercent);
  els.ocrAreaThresholdValue.textContent = `${areaThresholdPercent}%`;
  const nsfwPct = Math.round((library?.nsfwScoreThreshold ?? 0.45) * 100);
  els.nsfwThresholdInput.value = String(nsfwPct);
  els.nsfwThresholdValue.textContent = `${nsfwPct}%`;
  renderManualFolderList();
}

async function syncNsfwModelHint() {
  try {
    const info = await window.categorizerAPI.getNsfwModelInfo();
    if (info.exists) {
      els.nsfwModelHint.textContent = `Model loaded: ${info.path}`;
      els.nsfwModelHint.style.color = '';
      els.downloadNsfwModelButton.classList.add('hidden');
    } else {
      els.nsfwModelHint.textContent =
        `Model not installed. Press Download Model to install 320n.onnx to: ${info.path}`;
      els.nsfwModelHint.style.color = 'var(--danger)';
      els.downloadNsfwModelButton.classList.remove('hidden');
      els.downloadNsfwModelButton.disabled = false;
    }
    els.analyzeNsfwCheckLabel.title = info.exists
      ? 'Run NudeNet explicit content detection, classifying images as Safe / Explicit'
      : `NudeNet model not found — download 320n.onnx to: ${info.path}`;
  } catch {
    // non-fatal
  }
}

async function downloadNsfwModel() {
  els.downloadNsfwModelButton.disabled = true;
  els.nsfwModelHint.textContent = 'Downloading NudeNet package from PyPI...';
  els.nsfwModelHint.style.color = '';
  els.nsfwModelReport.textContent = [
    'Download started.',
    'Source: NudeNet 3.4.2 PyPI wheel',
    'Next: extract bundled nudenet/320n.onnx',
  ].join('\n');
  els.nsfwModelReport.classList.remove('hidden');
  try {
    const result = await window.categorizerAPI.downloadNsfwModel();
    const info = result.info;
    els.nsfwModelHint.textContent = `Model loaded: ${info.path}`;
    els.downloadNsfwModelButton.classList.add('hidden');
    els.nsfwModelReport.textContent = [
      'Download complete.',
      `Package: ${formatBytes(result.downloadedBytes)}`,
      `Model: ${formatBytes(result.modelBytes)}`,
      `Installed: ${info.path}`,
      `Source: ${result.sourceUrl}`,
      '',
      ...(result.report || []),
    ].join('\n');
    showToast('NudeNet model installed.');
  } catch (error) {
    els.downloadNsfwModelButton.disabled = false;
    const message = errorText(error);
    els.nsfwModelHint.textContent = 'Download failed. See report below.';
    els.nsfwModelHint.style.color = 'var(--danger)';
    els.nsfwModelReport.textContent = [
      'Download failed.',
      message,
      '',
      'No model was installed. You can try Download Model again.',
    ].join('\n');
    showToast(message);
  }
}

function formatAutoRefreshStatus(autoRefresh) {
  if (!autoRefresh) return '';
  const parts = [];
  parts.push(autoRefresh.taskInstalled ? 'Scheduled task: installed.' : 'Scheduled task: not installed.');
  if (autoRefresh.lastRunAt) {
    parts.push(`Last run: ${formatDate(Date.parse(autoRefresh.lastRunAt))} — ${autoRefresh.lastRunSummary || ''}`);
  } else {
    parts.push('Last run: never.');
  }
  return parts.join(' ');
}

function renderAutoRefreshRootList() {
  const knownRoots = state.settings?.knownRoots || [];
  const selected = new Set(state.autoRefresh?.roots || []);
  els.autoRefreshRootList.innerHTML = '';
  if (!knownRoots.length) {
    const empty = document.createElement('div');
    empty.className = 'manual-folder-empty';
    empty.textContent = 'No known root folders yet — choose a root folder first.';
    els.autoRefreshRootList.append(empty);
    return;
  }
  for (const entry of knownRoots) {
    const row = document.createElement('label');
    row.className = 'auto-refresh-root-row';
    const checkbox = document.createElement('input');
    checkbox.type = 'checkbox';
    checkbox.value = entry.path;
    checkbox.checked = selected.has(entry.path);
    checkbox.addEventListener('change', saveAutoRefreshSettings);
    const label = document.createElement('span');
    label.textContent = entry.path;
    label.title = entry.path;
    row.append(checkbox, label);
    els.autoRefreshRootList.append(row);
  }
}

function syncAutoRefreshDialog() {
  const autoRefresh = state.autoRefresh;
  if (!autoRefresh) return;
  els.autoRefreshEnabledInput.checked = autoRefresh.enabled;
  els.autoRefreshTimeInput.value = autoRefresh.time;
  els.autoRefreshNsfwInput.checked = autoRefresh.runNsfw;
  els.autoRefreshTextAnalysisInput.checked = autoRefresh.runTextAnalysis;
  els.autoRefreshTextExtractionInput.checked = autoRefresh.runTextExtraction;
  els.autoRefreshLowPriorityInput.checked = autoRefresh.lowPriority;
  els.autoRefreshToastInput.checked = autoRefresh.toast;
  els.autoRefreshOptions.classList.toggle('disabled-section', !autoRefresh.enabled);
  els.autoRefreshStatus.textContent = formatAutoRefreshStatus(autoRefresh);
  renderAutoRefreshRootList();
}

async function loadAutoRefreshSettings() {
  try {
    state.autoRefresh = await window.categorizerAPI.getAutoRefreshSettings();
  } catch (error) {
    state.autoRefresh = null;
    showToast(errorText(error));
  }
  syncAutoRefreshDialog();
}

function collectCheckedAutoRefreshRoots() {
  return [...els.autoRefreshRootList.querySelectorAll('input[type="checkbox"]:checked')].map(input => input.value);
}

async function saveAutoRefreshSettings() {
  const payload = {
    enabled: els.autoRefreshEnabledInput.checked,
    time: els.autoRefreshTimeInput.value || '04:00',
    roots: collectCheckedAutoRefreshRoots(),
    runNsfw: els.autoRefreshNsfwInput.checked,
    runTextAnalysis: els.autoRefreshTextAnalysisInput.checked,
    runTextExtraction: els.autoRefreshTextExtractionInput.checked,
    lowPriority: els.autoRefreshLowPriorityInput.checked,
    toast: els.autoRefreshToastInput.checked,
  };
  els.autoRefreshOptions.classList.toggle('disabled-section', !payload.enabled);
  try {
    state.autoRefresh = await window.categorizerAPI.setAutoRefreshSettings(payload);
    els.autoRefreshStatus.textContent = formatAutoRefreshStatus(state.autoRefresh);
  } catch (error) {
    showToast(errorText(error));
  }
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

function includedSourceFolderNames() {
  const folders = state.library?.sourceFolders || [];
  if (!folders.length) return null;
  return new Set(folders.filter(folder => folder.includedInAnalysis).map(folder => folder.name));
}

function imagesInIncludedSourceFolders(images = state.library?.images || []) {
  const included = includedSourceFolderNames();
  if (!included) return images;
  return images.filter(image => included.has(image.sourceFolder));
}

function categoryCountsForIncludedSources() {
  const counts = new Map();
  let unclassified = 0;
  for (const image of imagesInIncludedSourceFolders()) {
    if (image.category) {
      counts.set(image.category, (counts.get(image.category) || 0) + 1);
    } else {
      unclassified += 1;
    }
  }
  return { counts, unclassified };
}

function visibleImages() {
  const library = state.library;
  if (!library) return [];

  let images = imagesInIncludedSourceFolders(library.images);
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

function renderRootFolderSelect() {
  const knownRoots = state.settings?.knownRoots || [];
  const currentRoot = state.library?.root || state.settings?.lastRoot || '';

  els.rootFolderSelect.innerHTML = '';
  if (!knownRoots.length) {
    const empty = document.createElement('option');
    empty.value = '';
    empty.textContent = 'No folder chosen';
    els.rootFolderSelect.append(empty);
  }
  for (const entry of knownRoots) {
    const option = document.createElement('option');
    option.value = entry.path;
    option.textContent = entry.exists ? shortPath(entry.path) : `${shortPath(entry.path)} (not found)`;
    els.rootFolderSelect.append(option);
  }
  const addOption = document.createElement('option');
  addOption.value = '__add__';
  addOption.textContent = '+ Add Folder...';
  els.rootFolderSelect.append(addOption);

  els.rootFolderSelect.value = currentRoot;
}

function renderSettings() {
  if (!state.settings) return;
  applyUiSettings();
  renderRootFolderSelect();
  syncSettingsDialog();
}

function renderSidebar() {
  const library = state.library;
  const includedImages = imagesInIncludedSourceFolders();
  const { counts: categoryCounts, unclassified } = categoryCountsForIncludedSources();
  const allCount = includedImages.length;
  const unclassifiedCount = unclassified;

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
    empty.disabled = state.analyzing;
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
      button.querySelector('.count-pill').textContent = String(categoryCounts.get(category.name) || 0);
      button.addEventListener('click', () => selectCategory(category.name));

      const renameButton = document.createElement('button');
      renameButton.type = 'button';
      renameButton.className = 'category-rename-button';
      renameButton.title = `Rename ${category.name}`;
      renameButton.textContent = 'Rename';
      renameButton.disabled = state.analyzing;
      renameButton.addEventListener('click', event => {
        event.stopPropagation();
        openCategoryRenameDialog(category.name);
      });

      const deleteButton = document.createElement('button');
      deleteButton.type = 'button';
      deleteButton.className = 'category-rename-button';
      deleteButton.title = `Delete ${category.name}`;
      deleteButton.textContent = 'Delete';
      deleteButton.disabled = state.analyzing;
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
      row.innerHTML = `
        <label class="source-folder-include" title="Include this folder in browsing and analysis">
          <input type="checkbox" class="source-folder-checkbox">
        </label>
        <span class="category-name"></span>
        <span class="count-pill"></span>
      `;
      const checkbox = row.querySelector('.source-folder-checkbox');
      checkbox.checked = folder.includedInAnalysis;
      checkbox.disabled = state.analyzing;
      checkbox.addEventListener('change', () => setFolderAnalysisIncluded(folder.name, checkbox.checked));
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

function percent(value) {
  if (value == null || Number.isNaN(Number(value))) return null;
  return `${Math.round(Number(value) * 100)}%`;
}

function analysisSummary(image) {
  const lines = [];
  if (image.nsfwScore != null) {
    const threshold = state.library?.nsfwScoreThreshold ?? 0.45;
    const status = image.nsfwScore >= threshold ? 'Explicit' : 'Below explicit threshold';
    const labels = Array.isArray(image.nsfwLabels) && image.nsfwLabels.length
      ? image.nsfwLabels.join(' · ')
      : 'No NudeNet labels recorded';
    lines.push(`NudeNet: ${status} (${percent(image.nsfwScore)}; threshold ${percent(threshold)})`);
    lines.push(labels);
  } else {
    lines.push('NudeNet: not analyzed');
  }

  if (image.ocrWordCount != null && image.ocrTextAreaRatio != null) {
    lines.push(`Text: ${image.ocrWordCount} words · ${percent(image.ocrTextAreaRatio)} area`);
  } else {
    lines.push('Text: not analyzed');
  }

  if (image.ocrTextChars != null) {
    lines.push(image.ocrTextChars > 0
      ? `Extracted text: ${image.ocrTextChars} chars saved`
      : 'Extracted text: no text found');
  }

  if (image.classifiedBy) {
    lines.push(`Classification: ${image.category || 'Unclassified'} (${image.classifiedBy})`);
  }

  return lines;
}

function buildImageCard(image) {
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
      <div class="analysis-summary"></div>
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

  card.querySelector('.thumb-image').src = window.categorizerAPI.getFileUrl(image.thumbnailPath || image.path);
  card.querySelector('.file-title').textContent = image.name;
  const folderText = image.sourceFolder ? ` · ${image.sourceFolder}` : '';
  const classifiedBy = image.classifiedBy;
  const badge = classifiedBy === 'manual' ? ' · manual'
    : (classifiedBy === 'auto' || classifiedBy === 'auto-nsfw') ? ' · auto'
    : '';
  const nsfwBadge = image.nsfwScore != null
    ? ` · ${Math.round(image.nsfwScore * 100)}% explicit`
    : '';
  card.querySelector('.file-meta').textContent =
    `${formatDate(image.modifiedMs)} · ${formatBytes(image.size)}${folderText}${nsfwBadge}${badge}`;
  const summaryLines = analysisSummary(image);
  const summary = card.querySelector('.analysis-summary');
  summary.textContent = summaryLines.join('\n');
  summary.title = summaryLines.join('\n');

  const select = card.querySelector('.category-select');
  select.innerHTML = categoryOptionsHtml(image.category);
  select.disabled = state.analyzing;
  select.addEventListener('change', () => assignCategory(image.hash, select.value || null));

  const moveButton = card.querySelector('.move-button');
  moveButton.disabled = state.analyzing;
  moveButton.addEventListener('click', () => openMoveDialog(image));
  card.querySelector('.open-button').addEventListener('click', () => openImage(image.path));
  card.querySelector('.reveal-button').addEventListener('click', () => revealImage(image.path));
  card.addEventListener('pointerdown', event => startPointerDrag(event, card));

  return card;
}

const GRID_ROW_GAP = 14;
const VIRTUAL_BUFFER_ROWS = 3;

function estimatedCardHeight() {
  return state.cardHeight || tileSize() + 92;
}

function computeGridColumns() {
  const gridWidth = els.imageGrid.clientWidth || els.mainDropTarget.clientWidth;
  const tile = tileSize();
  return Math.max(1, Math.floor((gridWidth + GRID_ROW_GAP) / (tile + GRID_ROW_GAP)));
}

function computeVirtualWindow(totalImages) {
  const columns = computeGridColumns();
  const rowHeight = estimatedCardHeight() + GRID_ROW_GAP;
  const totalRows = Math.ceil(totalImages / columns);
  const scrollTop = els.mainDropTarget.scrollTop;
  const viewportHeight = els.mainDropTarget.clientHeight;
  const bufferPx = rowHeight * VIRTUAL_BUFFER_ROWS;

  const startRow = Math.max(0, Math.floor((scrollTop - bufferPx) / rowHeight));
  const endRow = Math.min(totalRows, Math.ceil((scrollTop + viewportHeight + bufferPx) / rowHeight));

  return {
    startIndex: startRow * columns,
    endIndex: Math.min(totalImages, endRow * columns),
    topPadding: startRow * rowHeight,
    bottomPadding: (totalRows - endRow) * rowHeight,
  };
}

function renderVirtualWindow(images, window_) {
  state.virtualImages = images;
  state.virtualStart = window_.startIndex;
  state.virtualEnd = window_.endIndex;
  els.imageGrid.style.paddingTop = `${window_.topPadding}px`;
  els.imageGrid.style.paddingBottom = `${window_.bottomPadding}px`;
  els.imageGrid.innerHTML = '';

  for (const image of images.slice(window_.startIndex, window_.endIndex)) {
    els.imageGrid.append(buildImageCard(image));
  }

  if (!state.cardHeight) {
    const firstCard = els.imageGrid.querySelector('.image-card');
    if (firstCard) state.cardHeight = firstCard.getBoundingClientRect().height;
  }
}

function onGridScroll() {
  if (state.scrollFrameRequested) return;
  state.scrollFrameRequested = true;
  requestAnimationFrame(() => {
    state.scrollFrameRequested = false;
    const images = state.virtualImages;
    if (!images || !images.length) return;
    const window_ = computeVirtualWindow(images.length);
    if (window_.startIndex === state.virtualStart && window_.endIndex === state.virtualEnd) return;
    renderVirtualWindow(images, window_);
  });
}

function renderImages() {
  const images = visibleImages();
  els.emptyState.classList.toggle('visible', images.length === 0);

  if (!images.length) {
    state.virtualImages = null;
    els.imageGrid.style.paddingTop = '0px';
    els.imageGrid.style.paddingBottom = '0px';
    els.imageGrid.innerHTML = '';
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

  renderVirtualWindow(images, computeVirtualWindow(images.length));
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
  setStatus('Scanning for new, moved, or deleted images…');
  try {
    await loadSettings();
    await refreshLibrary();
  } catch (error) {
    showToast(errorText(error));
    state.settings = {
      lastRoot: null,
      lastRootExists: false,
      tileSize: 168,
      darkMode: true,
      knownRoots: [],
    };
    state.library = null;
  }
  if (state.currentView === 'category' && !(state.library?.categories || []).some(c => c.name === state.currentCategory)) {
    state.currentView = 'all';
    state.currentCategory = null;
  }
  render();
  if (state.library) {
    setStatus(`Up to date — ${imageCountLabel(state.library.images.length)}.`);
  } else if (state.settings?.lastRoot) {
    setStatus('Could not load the selected folder.');
  } else {
    setStatus('');
  }
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
  const folders = (state.library?.sourceFolders || []).filter(folder => folder.name !== 'Root');
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
  syncNsfwModelHint();
  loadAutoRefreshSettings();
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

function syncTextThresholdLabels() {
  els.ocrWordThresholdValue.textContent = `${els.ocrWordThresholdInput.value} words`;
  els.ocrAreaThresholdValue.textContent = `${els.ocrAreaThresholdInput.value}%`;
}

function syncNsfwThresholdLabel() {
  els.nsfwThresholdValue.textContent = `${els.nsfwThresholdInput.value}%`;
}

async function saveTextThresholds() {
  if (!state.library) return;
  const wordThreshold = Number(els.ocrWordThresholdInput.value);
  const areaThreshold = Number(els.ocrAreaThresholdInput.value) / 100;
  try {
    state.library = await window.categorizerAPI.setTextThresholds(state.library.root, wordThreshold, areaThreshold);
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

async function saveNsfwThreshold() {
  if (!state.library) return;
  const threshold = Number(els.nsfwThresholdInput.value) / 100;
  try {
    state.library = await window.categorizerAPI.setNsfwThreshold(state.library.root, threshold);
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

async function setFolderAnalysisIncluded(folderName, included) {
  if (!state.library) return;
  try {
    state.library = await window.categorizerAPI.setFolderAnalysisIncluded(state.library.root, folderName, included);
    render();
  } catch (error) {
    showToast(errorText(error));
  }
}

// ==============================
// Unified analysis queue
// ==============================

function setInteractionsLocked(locked) {
  state.analyzing = locked;
  els.addCategoryButton.disabled = locked;
  els.addSourceFolderButton.disabled = locked;
  els.rootFolderSelect.disabled = locked;
  els.refreshButton.disabled = locked;
  els.openFolderButton.disabled = locked;
  els.settingsButton.disabled = locked;
  els.analyzeButton.classList.toggle('hidden', locked);
  els.reanalyzeButton.classList.toggle('hidden', locked);
  els.cancelAnalysisButton.classList.toggle('hidden', !locked);
  render();
}

function analysisTypeLabel(type) {
  if (type === 'nsfw') return 'Explicit';
  if (type === 'ocr') return 'Extract Text';
  return 'Text';
}

async function runNextInQueue() {
  if (!state.analysisQueue.length) {
    // All done — refresh and unlock
    state.analysisRunning = null;
    setInteractionsLocked(false);
    if (state.library) {
      try {
        setStatus('Refreshing library…');
        state.library = await window.categorizerAPI.scanLibrary(state.library.root);
        render();
        setStatus('Analysis complete.');
      } catch (error) {
        setStatus('');
        showToast(errorText(error));
      }
    }
    return;
  }

  const { type, force } = state.analysisQueue.shift();
  state.analysisRunning = type;
  const verb = force ? 'Re-analyzing' : 'Analyzing';
  setStatus(`${verb} (${analysisTypeLabel(type)})…`);

  try {
    if (type === 'text') {
      await window.categorizerAPI.analyzeText(state.library.root, force);
    } else if (type === 'ocr') {
      await window.categorizerAPI.extractText(state.library.root, force);
    } else {
      await window.categorizerAPI.analyzeNsfw(state.library.root, force);
    }
  } catch (error) {
    showToast(errorText(error));
    // Skip to next
    await runNextInQueue();
  }
}

async function startAnalysis(force) {
  if (!state.library) {
    showToast('Choose a root folder first.');
    return;
  }
  const wantText = els.analyzeTextCheck.checked;
  const wantNsfw = els.analyzeNsfwCheck.checked;
  const wantOcr = els.extractTextCheck.checked;
  if (!wantText && !wantNsfw && !wantOcr) {
    showToast('Select at least one analysis type.');
    return;
  }

  state.analysisQueue = [];
  if (wantNsfw) state.analysisQueue.push({ type: 'nsfw', force });
  if (wantText) state.analysisQueue.push({ type: 'text', force });
  if (wantOcr) state.analysisQueue.push({ type: 'ocr', force });

  setInteractionsLocked(true);
  await runNextInQueue();
}

async function cancelCurrentAnalysis() {
  // Cancel the running one; the queue will be drained when the finished event fires
  state.analysisQueue = [];
  try {
    if (state.analysisRunning === 'text') {
      await window.categorizerAPI.cancelTextAnalysis();
    } else if (state.analysisRunning === 'nsfw') {
      await window.categorizerAPI.cancelNsfwAnalysis();
    } else if (state.analysisRunning === 'ocr') {
      await window.categorizerAPI.cancelTextExtraction();
    }
    setStatus('Cancelling…');
  } catch (error) {
    showToast(errorText(error));
  }
}

async function onAnalysisFinished(type, { status, message }) {
  if (state.analysisRunning !== type) return; // stale event

  if (status === 'error') {
    state.analysisQueue = [];
    state.analysisRunning = null;
    setInteractionsLocked(false);
    setStatus('');
    showToast(message || `${analysisTypeLabel(type)} analysis failed`);
    // Still refresh so partial results are visible
    if (state.library) {
      try {
        state.library = await window.categorizerAPI.scanLibrary(state.library.root);
        render();
      } catch { /* ignore */ }
    }
    return;
  }

  if (status === 'cancelled') {
    state.analysisQueue = [];
    state.analysisRunning = null;
    setInteractionsLocked(false);
    const summary = `${analysisTypeLabel(type)} analysis cancelled.`;
    showToast(summary);
    if (state.library) {
      try {
        state.library = await window.categorizerAPI.scanLibrary(state.library.root);
        render();
        setStatus(summary);
      } catch { setStatus(''); }
    } else {
      setStatus(summary);
    }
    return;
  }

  if (message) showToast(message);
  // Move to next item in queue
  await runNextInQueue();
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
  state.cardHeight = null;
  renderImages();
}

async function changeRootFolder() {
  setStatus('Loading folder…');
  try {
    const library = await window.categorizerAPI.chooseRootFolder(state.library?.root);
    if (!library) {
      setStatus('');
      return;
    }
    state.library = library;
    state.settings = await window.categorizerAPI.getSettings();
    state.currentView = 'all';
    state.currentCategory = null;
    render();
    setStatus(`Loaded ${imageCountLabel(state.library.images.length)}.`);
  } catch (error) {
    setStatus('');
    showToast(errorText(error));
  }
}

async function selectRootFolder(rootPath) {
  if (!rootPath || rootPath === state.library?.root) return;
  setStatus('Loading folder…');
  try {
    state.library = await window.categorizerAPI.selectRootFolder(rootPath);
    state.settings = await window.categorizerAPI.getSettings();
    state.currentView = 'all';
    state.currentCategory = null;
    render();
    setStatus(`Loaded ${imageCountLabel(state.library.images.length)}.`);
  } catch (error) {
    setStatus('');
    showToast(errorText(error));
    renderRootFolderSelect();
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
  if (state.analyzing || event.button !== 0 || event.target.closest('button, select, .analysis-summary')) return;

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
  els.rootFolderSelect.addEventListener('change', () => {
    const value = els.rootFolderSelect.value;
    if (value === '__add__') {
      renderRootFolderSelect();
      changeRootFolder();
      return;
    }
    selectRootFolder(value);
  });
  els.settingsButton.addEventListener('click', openSettingsDialog);
  els.refreshButton.addEventListener('click', refreshAll);
  els.analyzeButton.addEventListener('click', () => startAnalysis(false));
  els.reanalyzeButton.addEventListener('click', () => startAnalysis(true));
  els.cancelAnalysisButton.addEventListener('click', cancelCurrentAnalysis);
  els.openFolderButton.addEventListener('click', openCurrentRootFolder);
  els.searchInput.addEventListener('input', () => {
    state.search = els.searchInput.value;
    renderImages();
  });
  els.sortSelect.addEventListener('change', () => {
    state.sort = els.sortSelect.value;
    renderImages();
  });
  els.mainDropTarget.addEventListener('scroll', onGridScroll, { passive: true });
  window.addEventListener('resize', () => {
    clearTimeout(installEvents.resizeTimer);
    installEvents.resizeTimer = setTimeout(() => {
      state.cardHeight = null;
      renderImages();
    }, 120);
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
  els.ocrWordThresholdInput.addEventListener('input', syncTextThresholdLabels);
  els.ocrWordThresholdInput.addEventListener('change', saveTextThresholds);
  els.ocrAreaThresholdInput.addEventListener('input', syncTextThresholdLabels);
  els.ocrAreaThresholdInput.addEventListener('change', saveTextThresholds);
  els.nsfwThresholdInput.addEventListener('input', syncNsfwThresholdLabel);
  els.nsfwThresholdInput.addEventListener('change', saveNsfwThreshold);
  els.downloadNsfwModelButton.addEventListener('click', downloadNsfwModel);
  els.autoRefreshEnabledInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshTimeInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshNsfwInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshTextAnalysisInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshTextExtractionInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshLowPriorityInput.addEventListener('change', saveAutoRefreshSettings);
  els.autoRefreshToastInput.addEventListener('change', saveAutoRefreshSettings);

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

let windowShown = false;

function showWindowAfterPaint() {
  if (windowShown) return;
  windowShown = true;

  requestAnimationFrame(() => {
    requestAnimationFrame(() => {
      window.categorizerAPI.showWindow?.()?.catch?.(error => {
        console.warn('Failed to show main window:', error);
      });
    });
  });
}

async function installAnalysisListeners() {
  const listeners = [
    window.categorizerAPI.onTextAnalysisProgress(({ processed, total, currentName }) => {
      setStatus(`Text: ${processed}/${total} — ${currentName}`);
    }),
    window.categorizerAPI.onTextAnalysisFinished(payload => onAnalysisFinished('text', payload)),
    window.categorizerAPI.onNsfwAnalysisProgress(({ processed, total, currentName }) => {
      setStatus(`Explicit: ${processed}/${total} — ${currentName}`);
    }),
    window.categorizerAPI.onNsfwAnalysisFinished(payload => onAnalysisFinished('nsfw', payload)),
    window.categorizerAPI.onTextExtractionProgress(({ processed, total, currentName }) => {
      setStatus(`Extract Text: ${processed}/${total} — ${currentName}`);
    }),
    window.categorizerAPI.onTextExtractionFinished(payload => onAnalysisFinished('ocr', payload)),
  ];

  const results = await Promise.allSettled(listeners);
  for (const result of results) {
    if (result.status === 'rejected') {
      console.warn('Failed to install analysis listener:', result.reason);
    }
  }
}

async function init() {
  try {
    installEvents();
    render();
    showWindowAfterPaint();

    await installAnalysisListeners();
    await refreshAll();
  } catch (error) {
    console.error('Startup failed:', error);
    setStatus('Startup hit an error. Choose or rescan a folder to retry.');
    showToast(errorText(error));
    render();
    showWindowAfterPaint();
  }
}

init();
