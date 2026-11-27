<script>
  import { onMount, onDestroy } from 'svelte'
  import { status, fpsHistory, connected } from './stores.js'
  import { fmtDuration } from './lib/fmt.js'
  import { connect, postCmd, selectDevice } from './lib/api.js'
  import StatusBar from './lib/StatusBar.svelte'
  import TargetCard from './lib/TargetCard.svelte'
  import EventLog from './lib/EventLog.svelte'
  import ThemeToggle from './lib/ThemeToggle.svelte'
  import RateChart from './lib/RateChart.svelte'
  let es
  onMount(() => { es = connect() })
  onDestroy(() => es && es.close())
  function reboot(){ if(confirm('Reboot the device? FreeD output stops briefly.')) postCmd('reboot').then(r=>alert(r)) }
  $: heard = $connected && $status.signal === 'ok'
  $: frozen = !$connected || $status.signal === 'lost'
  // The tab's title carries the alarm to someone looking at another tab.
  $: document.title = frozen ? '(!) NO SIGNAL - XRFD Dashboard' : 'XRFD Dashboard'
  function onDeviceChange(e) {
    selectDevice(e.currentTarget.value).then(ok => {
      if (!ok) alert('Could not switch device — it may no longer be on the network.')
    })
  }
</script>

<h1>
  <span class="dot" class:live={heard} class:dead={!heard}></span>
  XRFD Dashboard
  <span class="dev">
    device:
    {#if ($status.devices?.length ?? 0) > 1}
      <select value={$status.deviceIp} on:change={onDeviceChange}>
        {#each $status.devices as d (d.ip)}
          <option value={d.ip}>{d.ip}{d.live ? '' : ` — no signal ${fmtDuration(d.ageSec)}`}</option>
        {/each}
      </select>
    {:else}
      {$status.deviceIp || '-'}
    {/if}
  </span>
  <ThemeToggle/>
</h1>
{#if !$connected}
  <div class="alarm" role="alert">
    <b>DASHBOARD SERVER NOT RESPONDING</b>
    This page has stopped updating. Everything below is the last it received. Reconnecting...
  </div>
{:else if $status.signal === 'lost'}
  <div class="alarm" role="alert">
    <b>NO SIGNAL FROM {$status.deviceIp}</b>
    Last status {$status.lastSeen}, {fmtDuration($status.silentSec)} ago.
    Everything below is the last received, not the present.
  </div>
{:else if $status.signal === 'none'}
  <div class="alarm waiting">
    <b>NO DEVICE HEARD YET</b>
    Listening since {$status.listeningSince}.
  </div>
{/if}
<div class:stale={frozen}>
  <StatusBar s={$status}/>
  <RateChart history={$fpsHistory}/>
  <div class="grid">
    {#each $status.targets as t}<TargetCard {t}/>{:else}<div class="card">No target info yet...</div>{/each}
  </div>
</div>
<div class="foot"><h2>Event log</h2><button class="b-reboot" on:click={reboot}>Reboot device</button></div>
<EventLog log={$status.log}/>
