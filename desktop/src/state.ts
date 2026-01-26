import { proxy } from 'valtio';

export const state = proxy({
  ver: '',
  showUpdate: false,
  updateUrl: '',
  cwd: '',
  ip: '',
  dest: '',
  rarMode: 'rebuild',
  rarTemp: '',
  port: '9090',
  pending: false,
  progress: {},
  managePending: false,
  manageStep: 'inactive',
  manageFile: '',
  manageTotal: 0,
  manageProcessed: 0,
  manageCancel: false
});