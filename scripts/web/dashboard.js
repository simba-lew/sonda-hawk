/* Local assets only; telemetry and charts work without internet. */
(() => {
  'use strict';
  const el = id => document.getElementById(id);
  const history = {rs41: [], tbeam: []}, latest = {};
  let activeTab = 'tbeam';
  const visible = source => activeTab === source || (activeTab === 'compare' && el('show-'+source).checked);
  const colors = {rs41: '#69a7ff', tbeam: '#13bba4', delta:'#e7a436'};
  const metrics = [
    ['altitude', 'Wysokość [m AMSL]'], ['internal_temperature', 'Temperatura wewnętrzna · RS41 i T-Beam [°C]'],
    ['external_temperature', 'Temperatura zewnętrzna · RS41 [°C]'], ['delta_temperature', 'Różnica: wnętrze T-Beam − otoczenie RS41 [°C]'],
    ['pressure', 'Ciśnienie [hPa]'], ['humidity', 'Wilgotność [%]'],
    ['ascent_rate', 'Prędkość pionowa [m/s]'], ['snr', 'SNR odbioru [dB]'],
    ['battery', 'Napięcie baterii RS41 [V]']
  ];
  let radio = {}, prediction = null, busy = false, error = '', dirty = true, mapDirty = true;
  const fmt = (n, digits=1) => typeof n === 'number' && Number.isFinite(n) ? n.toFixed(digits) : '—';
  const age = t => t ? Math.max(0, (Date.now()/1000-t)) : null;
  const canvases = metrics.map(([key,title]) => {
    const section = document.createElement('div'); section.className='chartcard';
    const h=document.createElement('h2'); h.textContent=title;
    const canvas=document.createElement('canvas'); canvas.className='plot'; canvas.setAttribute('aria-label',title);
    section.append(h,canvas); el('charts').append(section); return [key,canvas];
  });
  const map = L.map('flightmap', {preferCanvas:true}).setView([52.18,21.18],10);
  const tiles=L.tileLayer('https://tile.openstreetmap.org/{z}/{x}/{y}.png',{
    maxZoom:19,attribution:'© <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a>'});
  const tracks={}, markers={};
  for (const s of Object.keys(history)) {
    tracks[s]=L.polyline([], {color:colors[s],weight:3}).addTo(map);
    markers[s]=null;
  }
  let predictionLayers=[];
  el('map-online').onchange=()=> el('map-online').checked ? tiles.addTo(map) : map.removeLayer(tiles);
  function bounds() {
    const layers=Object.values(tracks).filter(l=>l.getLatLngs().length).concat(predictionLayers.filter(l=>map.hasLayer(l)));
    if(layers.length) map.fitBounds(L.featureGroup(layers).getBounds(),{padding:[30,30],maxZoom:14});
  }
  el('fit-map').onclick=bounds;
  function popup(text){const d=document.createElement('div');d.textContent=text;return d;}
  function renderMap() {
    for(const s of Object.keys(history)) {
      const pts=visible(s)?history[s].filter(d=>d.fix).map(d=>[d.latitude,d.longitude]):[];
      tracks[s].setLatLngs(pts);
      if(markers[s] && (!visible(s) || !pts.length)) map.removeLayer(markers[s]);
      if(!pts.length) continue;
      if(!markers[s]) markers[s]=L.circleMarker(pts.at(-1),{radius:7,color:colors[s],fillOpacity:1}).addTo(map);
      else markers[s].setLatLng(pts.at(-1));
      if(!map.hasLayer(markers[s])) markers[s].addTo(map);
      const d=history[s].filter(d=>d.fix).at(-1);
      markers[s].bindPopup(popup(`${d.callsign}: ${fmt(d.altitude,0)} m AMSL · ${new Date(d.received_at*1000).toLocaleTimeString()}`));
    }
    if(el('follow-map').checked) {
      const valid=Object.values(latest).filter(d=>d.fix && visible(d.source)).sort((a,b)=>b.received_at-a.received_at);
      if(valid.length) map.panTo([valid[0].latitude,valid[0].longitude]);
    }
    for(const layer of predictionLayers) {
      if(prediction && visible(prediction.source)) {
        if(!map.hasLayer(layer)) layer.addTo(map);
      } else if(map.hasLayer(layer)) map.removeLayer(layer);
    }
  }
  function drawPrediction(p) {
    prediction=p;
    for(const layer of predictionLayers) map.removeLayer(layer);
    predictionLayers=[];
    for(const stage of p.stages) {
      const points=stage.trajectory.map(d=>[d.latitude,d.longitude]);
      predictionLayers.push(L.polyline(points,{color:'#e7a436',dashArray:'7 7',weight:3}).addTo(map));
    }
    const d=p.landing;
    predictionLayers.push(L.circleMarker([d.latitude,d.longitude],{color:'#e7a436',radius:10,fillOpacity:.35})
      .addTo(map).bindPopup(popup('Przewidywane lądowanie: '+new Date(d.datetime).toLocaleString())));
    updatePredictionText();
    mapDirty=true;
  }
  function updatePredictionText(){
    if(!prediction)return;
    const p=prediction, d=p.landing, seconds=(new Date(d.datetime)-Date.now())/1000;
    el('prediction-result').hidden=!visible(p.source);
    el('prediction-result').textContent=`${p.source==='rs41'?'RS41':'T-Beam'} · lądowanie: ${fmt(d.latitude,5)}, ${fmt(d.longitude,5)} · ${new Date(d.datetime).toLocaleString()}${seconds>0?' · za około '+Math.round(seconds/60)+' min':''} · obliczono ${new Date(p.created_at*1000).toLocaleTimeString()} (${Math.round(age(p.created_at))} s temu). Szacunek dla podanych parametrów lotu.`;
  }
  function reading(container,label,text) {
    const div=document.createElement('div'),small=document.createElement('small'),b=document.createElement('b');
    small.textContent=label; b.textContent=text;div.append(small,b);container.append(div);
  }
  function renderCards(){
    for(const [s,prefix] of [['rs41','rs'],['tbeam','tb']]) {
      const d=latest[s], container=el(prefix+'-readings'), t=d?age(d.received_at):null;
      el(prefix+'-age').textContent=t===null?'Brak danych':`${Math.round(t)} s temu${t>15?' · NIEAKTUALNE':''}`;
      el(prefix+'-age').style.color=t===null||t>15?'var(--bad)':'var(--ok)';
      container.replaceChildren();if(!d)continue;
      if(s==='rs41')el('rs-callsign').textContent=d.callsign;
      reading(container,'GPS',d.fix?'Pozycja poprawna':'BRAK FIX — pozycja pominięta');
      reading(container,'Pozycja',d.fix?`${fmt(d.latitude,5)}, ${fmt(d.longitude,5)}`:'—');
      for(const [key,label,unit] of [
        ['altitude','Wysokość',' m'],['temperature','Temp. zewnętrzna',' °C'],
        ['pressure','Ciśnienie',' hPa'],['humidity','Wilgotność',' %'],['ascent_rate','Prędkość pionowa',' m/s'],
        ['snr','SNR',' dB'],['internal_temperature','Temp. wewnętrzna RS41',' °C'],
        ['battery','Napięcie baterii RS41',' V'],['satellites','Satelity','']]){
        if(s==='tbeam'&&['internal_temperature','custom_temperature','battery','satellites'].includes(key))continue;
        const value=key==='temperature'&&s==='tbeam'?HawkeyeThermal.inside(d):d[key];
        const caption=key==='temperature'?(s==='tbeam'?'Temp. wnętrza gondoli':'Temp. otoczenia'):label;
        reading(container,caption,fmt(value,key==='battery'?3:1)+(value==null?'':unit));
      }
    }
    const a=radio.audio, fresh=a&&age(a.received_at)<3;
    el('rs-radio').textContent=(radio.detail||'Oczekiwanie na dekoder')+
      (a?` · dźwięk ${fresh?'OK':'NIEAKTUALNY'} · szczyt ${fmt(a.peak_dbfs)} dBFS${a.peak_dbfs>-1?' — zmniejsz poziom audio w SDRangel':''}`:'');
  }
  function renderCharts(){
    const now=Date.now()/1000, hour=el('chart-hour').checked;
    const differences=HawkeyeThermal.compare(history.tbeam,history.rs41);
    let sharedMin=Infinity,sharedMax=-Infinity;
    for(const [source,rows] of Object.entries(history))if(visible(source))for(const row of rows){
      if(hour&&row.received_at<=now-3600)continue;
      sharedMin=Math.min(sharedMin,row.received_at);sharedMax=Math.max(sharedMax,row.received_at);
    }
    const lastPair=differences.at(-1);
    el('thermal-summary').hidden=activeTab!=='compare';
    el('thermal-summary').textContent=lastPair&&age(lastPair.received_at)<=15&&visible('tbeam')&&visible('rs41')
      ?`ΔT wnętrze − otoczenie: ${fmt(lastPair.delta_temperature)} °C · odstęp odbioru próbek: ${fmt(lastPair.time_skew_s)} s.`
      :'ΔT: oczekiwanie na świeże pomiary obu źródeł (maksymalna różnica czasu odbioru 8 s).';
    const css=getComputedStyle(document.body),text=css.getPropertyValue('--muted'),line=css.getPropertyValue('--line');
    for(const [key,canvas] of canvases){
      canvas.parentElement.hidden=(key==='battery' && !visible('rs41')) ||
        (key==='external_temperature' && !visible('rs41')) ||
        (key==='delta_temperature' && !(activeTab==='compare'&&visible('tbeam')&&visible('rs41')));
      if(key==='internal_temperature') {
        const label=activeTab==='tbeam'?'Temperatura wewnętrzna · T-Beam [°C]':activeTab==='rs41'
          ?'Temperatura wewnętrzna · RS41 [°C]':'Temperatura wewnętrzna · RS41 i T-Beam [°C]';
        canvas.parentElement.querySelector('h2').textContent=label;canvas.setAttribute('aria-label',label);
      }
      if(canvas.parentElement.hidden)continue;
      const w=canvas.clientWidth,h=canvas.clientHeight,ratio=devicePixelRatio||1;
      canvas.width=w*ratio;canvas.height=h*ratio;
      const ctx=canvas.getContext('2d');ctx.scale(ratio,ratio);ctx.font='11px system-ui';
      const input=key==='delta_temperature'?{delta:differences}:HawkeyeThermal.chartSeries(key,history,visible);
      const series=Object.fromEntries(Object.entries(input).map(([s,rows])=>[s,rows.filter(d=>
        (!hour||d.received_at>now-3600)&&typeof d[key]==='number'&&Number.isFinite(d[key])&&
        (!['altitude','ascent_rate'].includes(key)||d.fix))]));
      const all=Object.values(series).flat();
      ctx.fillStyle=text;
      if(!all.length){ctx.fillText('Oczekiwanie na dane…',12,85);continue;}
      let xmin=Infinity,xmax=-Infinity,ymin=Infinity,ymax=-Infinity;
      for(const d of all){xmin=Math.min(xmin,d.received_at);xmax=Math.max(xmax,d.received_at);ymin=Math.min(ymin,d[key]);ymax=Math.max(ymax,d[key]);}
      xmin=Number.isFinite(sharedMin)?sharedMin:xmin;xmax=Math.max(sharedMax,xmin+10);const pad=Math.max((ymax-ymin)*.1,.1);ymin-=pad;ymax+=pad;
      const left=58,right=w-12,top=12,bottom=h-28;
      const x=t=>left+(t-xmin)/(xmax-xmin)*(right-left),y=v=>bottom-(v-ymin)/(ymax-ymin)*(bottom-top);
      ctx.strokeStyle=line;ctx.lineWidth=1;
      for(let i=0;i<4;i++){const v=ymin+(ymax-ymin)*i/3,py=y(v);ctx.beginPath();ctx.moveTo(left,py);ctx.lineTo(right,py);ctx.stroke();ctx.fillText(v.toFixed(Math.abs(v)<10?1:0),2,py+4);}
      ctx.fillText(new Date(xmin*1000).toLocaleTimeString(),left,h-6);
      const end=new Date(xmax*1000).toLocaleTimeString();ctx.fillText(end,Math.max(left,right-ctx.measureText(end).width),h-6);
      for(const [s,rows] of Object.entries(series)){
        ctx.strokeStyle=colors[s];ctx.fillStyle=colors[s];ctx.lineWidth=2;ctx.beginPath();let last=null;
        // Downsample for display to about one point per horizontal pixel.
        const stride=Math.max(1,Math.floor(rows.length/Math.max(w,1)));
        for(let i=0;i<rows.length;i+=stride){const d=rows[i];
          if(!last||d.received_at-last.received_at>30||last.callsign!==d.callsign)ctx.moveTo(x(d.received_at),y(d[key]));
          else ctx.lineTo(x(d.received_at),y(d[key]));last=d;
        }ctx.stroke();
        if(rows.length){const d=rows.at(-1);ctx.beginPath();ctx.arc(x(d.received_at),y(d[key]),2.5,0,Math.PI*2);ctx.fill();}
      }
    }
  }
  function ingest(d){
    if(!history[d.source])return;
    latest[d.source]=d;history[d.source].push(d);
    if(history[d.source].length>20000)history[d.source].shift();
    dirty=true;mapDirty=true;
  }
  es.addEventListener('snapshot',e=>{
    const d=JSON.parse(e.data);
    for(const s of Object.keys(history)){history[s]=d.history[s]||[];if(d.latest[s])latest[s]=d.latest[s];}
    radio=d.radio||{};busy=d.predict_busy;error=d.predict_error;
    if(d.prediction)drawPrediction(d.prediction);
    dirty=true;mapDirty=true;predictionStatus();
  });
  es.addEventListener('telemetry',e=>ingest(JSON.parse(e.data)));
  const rsLog=el('rs-log');
  es.addEventListener('line',e=>{
    const entry=JSON.parse(e.data);
    if(entry.source!=='rs41')return;
    const row=document.createElement('div'),stamp=document.createElement('span'),message=document.createElement('span');
    stamp.className='t';stamp.textContent=entry.t+'  ';message.textContent=entry.line;
    row.append(stamp,message);rsLog.append(row);
    while(rsLog.childElementCount>3000)rsLog.firstChild.remove();
    if(el('rs-follow').checked)rsLog.scrollTop=rsLog.scrollHeight;
  });
  el('rs-clear').onclick=()=>rsLog.replaceChildren();
  rsLog.addEventListener('scroll',()=>{if(rsLog.scrollTop+rsLog.clientHeight<rsLog.scrollHeight-8)el('rs-follow').checked=false;});
  es.addEventListener('radio',e=>{radio=JSON.parse(e.data);});
  es.addEventListener('prediction',e=>drawPrediction(JSON.parse(e.data)));
  es.addEventListener('prediction_status',e=>{const d=JSON.parse(e.data);busy=d.busy;error=d.error;predictionStatus();});
  function predictionStatus(){
    el('predict').disabled=busy;el('predict').textContent=busy?'Obliczanie…':'Oblicz predykcję';
    el('prediction-message').textContent=error?'Błąd: '+error+(prediction?' Ostatnia predykcja pozostaje na mapie.':''):
      busy?'Pobieranie predykcji z modelu wiatrów…':prediction?'Predykcja gotowa.':'Predykcja nie została jeszcze obliczona.';
  }
  async function predict(){
    if(busy)return;
    const payload={source:el('pred-source').value,phase:el('pred-phase').value,
      burst_altitude:Number(el('pred-burst').value),ascent_rate:Number(el('pred-ascent').value),descent_rate:Number(el('pred-descent').value)};
    busy=true;error='';predictionStatus();
    try{
      const r=await fetch('/predict',{method:'POST',headers:{'X-Token':TOKEN,'Content-Type':'application/json'},body:JSON.stringify(payload)});
      if(!r.ok)throw new Error(await r.text());
    }catch(e){busy=false;error=String(e);predictionStatus();}
  }
  el('prediction-form').onsubmit=e=>{e.preventDefault();predict();};
  el('pred-phase').onchange=()=>{const down=el('pred-phase').value==='descent';el('pred-burst').disabled=down;el('pred-ascent').disabled=down;};
  setInterval(()=>{if(el('pred-auto').checked)predict();},120000);
  window.addEventListener('resize',()=>{dirty=true;map.invalidateSize();});
  el('chart-hour').onchange=()=>{dirty=true;};
  function setTab(tab) {
    activeTab=tab;
    for(const button of document.querySelectorAll('[data-tab]')) {
      const selected=button.dataset.tab===tab;
      button.setAttribute('aria-selected',String(selected));
      button.tabIndex=selected?0:-1;
    }
    for(const section of document.querySelectorAll('[data-view]')) section.hidden=section.dataset.view!==tab;
    for(const section of document.querySelectorAll('[data-source]')) section.hidden=!visible(section.dataset.source);
    el('comparison-options').hidden=tab!=='compare';
    el('compare-log-hint').hidden=tab!=='compare';
    if(tab!=='compare')el('pred-source').value=tab;
    el('pred-source').disabled=tab!=='compare';
    dirty=true;mapDirty=true;
    renderCharts();renderMap();updatePredictionText();map.invalidateSize();
    if(el('follow').checked)document.getElementById('log').scrollTop=document.getElementById('log').scrollHeight;
    if(el('rs-follow').checked)rsLog.scrollTop=rsLog.scrollHeight;
  }
  const tabs=[...document.querySelectorAll('[data-tab]')];
  for(const button of tabs) {
    button.onclick=()=>setTab(button.dataset.tab);
    button.onkeydown=e=>{
      if(!['ArrowLeft','ArrowRight','Home','End'].includes(e.key))return;
      e.preventDefault();
      let i=tabs.indexOf(button);
      i=e.key==='Home'?0:e.key==='End'?tabs.length-1:(i+(e.key==='ArrowRight'?1:-1)+tabs.length)%tabs.length;
      setTab(tabs[i].dataset.tab);tabs[i].focus();
    };
  }
  el('show-tbeam').onchange=()=>setTab('compare');
  el('show-rs41').onchange=()=>setTab('compare');
  setInterval(()=>{renderCards();updatePredictionText();if(dirty){renderCharts();dirty=false;}if(mapDirty){renderMap();mapDirty=false;}},1000);
  renderCards();
  setTab('tbeam');
  const stream=new EventSource('/events');
  for(const type of ['line','status','snapshot','telemetry','radio','prediction','prediction_status']) {
    stream.addEventListener(type,e=>es.dispatchEvent(new MessageEvent(type,{data:e.data})));
  }
  stream.onerror=()=>{if(es.onerror)es.onerror();};
  window.addEventListener('beforeunload',()=>stream.close());
})();
