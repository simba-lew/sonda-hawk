/* Timing-aware thermal comparison. No interpolation or stale-value filling. */
(function(root) {
  'use strict';
  function inside(d) {
    return Number.isFinite(d.internal_temperature) ? d.internal_temperature : d.temperature;
  }
  function compare(tbeam, rs41, maxSkew=8) {
    const inner=tbeam.filter(d=>Number.isFinite(d.received_at)&&Number.isFinite(inside(d)));
    const outer=rs41.filter(d=>Number.isFinite(d.received_at)&&Number.isFinite(d.temperature));
    const points=[];
    let j=0;
    for(const a of inner) {
      while(j+1<outer.length && outer[j+1].received_at<=a.received_at)j++;
      const choices=[outer[j],outer[j+1]].filter(Boolean);
      choices.sort((b,c)=>Math.abs(a.received_at-b.received_at)-Math.abs(a.received_at-c.received_at));
      const b=choices[0];
      if(!b || Math.abs(a.received_at-b.received_at)>maxSkew)continue;
      points.push({source:'delta',received_at:a.received_at,delta_temperature:inside(a)-b.temperature,
        time_skew_s:Math.abs(a.received_at-b.received_at),callsign:'Wnętrze − otoczenie'});
    }
    return points;
  }
  const api={inside,compare};
  if(typeof module!=='undefined' && module.exports)module.exports=api;
  else root.HawkeyeThermal=api;
})(typeof globalThis!=='undefined'?globalThis:this);
