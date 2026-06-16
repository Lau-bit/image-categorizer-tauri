'use strict';

const tauri = window.__TAURI__;
const invoke = tauri?.core?.invoke;
const dialog = tauri?.dialog;
const convertFileSrc = tauri?.core?.convertFileSrc;

if (!invoke || !dialog || !convertFileSrc) {
  console.error('Tauri API is not available.');
}

window.categorizerAPI = {
  getSettings: () => invoke('get_app_settings'),
  setTileSize: tileSize => invoke('set_tile_size', { tileSize }),
  setDarkMode: darkMode => invoke('set_dark_mode', { darkMode }),

  scanLibrary: root => invoke('scan_library', { root }),
  setSourcePattern: (root, preset, regex) => invoke('set_source_pattern', { root, preset, regex }),
  addManualSourceFolder: (root, folderPath) => invoke('add_manual_source_folder', { root, folderPath }),
  removeManualSourceFolder: (root, folderName) => invoke('remove_manual_source_folder', { root, folderName }),

  createCategory: (root, name) => invoke('create_category', { root, name }),
  renameCategory: (root, oldName, newName) => invoke('rename_category', { root, oldName, newName }),
  deleteCategory: (root, name) => invoke('delete_category', { root, name }),

  assignCategory: (root, hash, category) => invoke('assign_category', { root, hash, category }),
  moveImage: (root, hash, targetFolder) => invoke('move_image', { root, hash, targetFolder }),

  openImage: filePath => invoke('open_image', { filePath }),
  revealImage: filePath => invoke('reveal_image', { filePath }),
  openRootFolder: root => invoke('open_root_folder', { root }),

  getFileUrl: filePath => convertFileSrc(filePath),

  chooseRootFolder: async currentPath => {
    const folderPath = await dialog.open({
      title: 'Choose Image Library Root Folder',
      defaultPath: currentPath || undefined,
      directory: true,
      multiple: false,
    });
    if (!folderPath) return null;
    return invoke('choose_root_folder', { folderPath });
  },

  chooseManualSourceFolder: async root => {
    const folderPath = await dialog.open({
      title: 'Choose a Source Subfolder',
      defaultPath: root || undefined,
      directory: true,
      multiple: false,
    });
    if (!folderPath) return null;
    return invoke('add_manual_source_folder', { root, folderPath });
  },
};
