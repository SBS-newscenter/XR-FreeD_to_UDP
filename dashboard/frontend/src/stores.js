import { writable } from 'svelte/store'

export const status = writable({
  deviceIp: '', devices: [], ageSec: 999999, live: false, up: 0, rx: 0, fps: 0,
  dhcpOk: 0, dhcpFail: 0, rtr: '?', conflict: false, targets: [], log: [],
  // '' until the first snapshot arrives, so the page opens without an alarm
  signal: '', lastSeen: '', silentSec: 0, listeningSince: '',
})
// false while the live stream from the dashboard server is down: the page is
// then showing its last snapshot, whatever the device is doing
export const connected = writable(true)
export const fpsHistory = writable([])   // for RateChart
