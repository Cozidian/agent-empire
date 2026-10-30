//! Spectator mode: a read-only web view of the empire for someone watching along.
//! It shows statuses, titles and missions, never an agent's screen, and only answers
//! requests carrying the token printed at startup.

use crate::proto::{Req, Resp, call};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

pub fn run(args: &[String]) -> std::io::Result<()> {
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let bind = opt("--bind").unwrap_or_else(|| "127.0.0.1".into());
    let port = opt("--port").unwrap_or_else(|| "7777".into());
    let token = token();
    let listener = TcpListener::bind(format!("{bind}:{port}"))?;
    let shown = if bind == "0.0.0.0" { hostname() } else { bind.clone() };
    println!("spectating at http://{shown}:{port}/?t={token}");
    if bind != "127.0.0.1" && bind != "localhost" {
        println!("(listening on {bind}: anyone who can reach this machine and has the link can watch)");
    }
    println!("read-only: agent statuses, titles and missions; never their screens. Ctrl-C to stop.");
    for s in listener.incoming().flatten() {
        let token = token.clone();
        thread::spawn(move || {
            let _ = serve(s, &token);
        });
    }
    Ok(())
}

fn serve(mut s: TcpStream, token: &str) -> std::io::Result<()> {
    let mut line = String::new();
    BufReader::new(&s).read_line(&mut line)?;
    let path = line.split_whitespace().nth(1).unwrap_or("/");
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let authorized = query.split('&').any(|kv| kv.strip_prefix("t=") == Some(token));
    let (status, kind, body) = match (route, authorized) {
        (_, false) => ("403 Forbidden", "text/plain", "spectating needs the link with its token\n".to_string()),
        ("/", true) => ("200 OK", "text/html; charset=utf-8", PAGE.to_string()),
        ("/state", true) => ("200 OK", "application/json", state()),
        _ => ("404 Not Found", "text/plain", "not found\n".to_string()),
    };
    write!(s, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}", body.len())
}

/// The empire as the page sees it: agents (without screens) and missions.
fn state() -> String {
    match call(&Req::Peek) {
        Ok(Resp::Agents { agents, missions }) => {
            let agents: Vec<serde_json::Value> = agents
                .iter()
                .map(|a| {
                    serde_json::json!({
                        "id": a.id, "kind": a.kind, "cwd": a.cwd, "title": a.title, "status": a.status,
                        "age": a.age, "turns": a.turns, "question": a.ask.as_ref().map(|q| q.question.clone()),
                        "conflict": !a.war.is_empty(),
                    })
                })
                .collect();
            let missions: Vec<serde_json::Value> =
                missions.iter().map(|m| serde_json::json!({ "cwd": m.cwd, "text": m.text, "state": m.state })).collect();
            serde_json::json!({ "online": true, "agents": agents, "missions": missions }).to_string()
        }
        _ => r#"{"online":false,"agents":[],"missions":[]}"#.into(),
    }
}

fn token() -> String {
    let mut b = [0u8; 12];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut f, &mut b);
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "localhost".into())
}

const PAGE: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Agent Empire</title>
<style>
:root{--bg:#070a14;--panel:#0b1020;--text:#d8dee9;--dim:#6b7a90;--line:#16233a}
*{box-sizing:border-box}html,body{margin:0;height:100%;background:var(--bg);color:var(--text);font:14px/1.45 ui-monospace,"JetBrains Mono",monospace}
#wrap{display:grid;grid-template-columns:1fr 360px;height:100%}
canvas{width:100%;height:100%;display:block}
aside{background:var(--panel);border-left:1px solid var(--line);overflow:auto;padding:14px 16px}
h1{font-size:13px;letter-spacing:.12em;color:#9fd8ff;margin:0 0 4px}h2{font-size:11px;letter-spacing:.14em;color:#9fd8ff;margin:18px 0 6px;border-bottom:1px solid var(--line);padding-bottom:4px}
.sub{color:var(--dim);font-size:12px}.row{display:flex;gap:8px;align-items:baseline;padding:2px 0}.row .t{color:var(--dim);overflow:hidden;text-overflow:ellipsis;white-space:nowrap;flex:1}
.w{margin-top:8px;color:#fff}.q{color:#ff8a7a;font-size:12px;margin-left:22px}
@media (max-width:760px){#wrap{grid-template-columns:1fr;grid-template-rows:55vh auto}aside{border-left:0;border-top:1px solid var(--line)}}
</style></head><body><div id="wrap"><canvas id="c"></canvas><aside>
<h1>◆ AGENT EMPIRE</h1><div class="sub" id="sum">connecting…</div>
<h2>FLEET</h2><div id="fleet"></div><h2>MISSIONS</h2><div id="missions" class="sub">none</div>
<p class="sub" style="margin-top:20px">Read-only spectator view. Agents' screens are never shown.</p>
</aside></div>
<script>
const COL={blocked:"#ff5a4a",done:"#5cff8a",working:"#4fd8ff",booting:"#ffc04a",idle:"#7f8ea3",exited:"#555"};
const KIND={claude:"#ff9a5a",codex:"#e8eef6",grok:"#c890ff"};
const GLYPH={blocked:"▲",done:"✔",working:"◆",booting:"◌",idle:"○",exited:"✝"};
let S={agents:[],missions:[],online:false};
const tok=new URLSearchParams(location.search).get("t");
const esc=s=>String(s??"").replace(/[&<>"]/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;"}[c]));
const base=p=>p.replace(/\/+$/,"").split("/").pop()||"/";
const parent=p=>p.replace(/\/+$/,"").split("/").slice(0,-1).join("/")||"/";
function hash(s){let h=2166136261;for(const ch of s)h=Math.imul(h^ch.charCodeAt(0),16777619);return (h>>>0)/4294967295}
async function poll(){try{const r=await fetch("/state?t="+tok,{cache:"no-store"});S=await r.json();side()}catch(e){S.online=false}setTimeout(poll,1000)}
function side(){
  const n=s=>S.agents.filter(a=>a.status===s).length;
  document.getElementById("sum").textContent=S.online?`${S.agents.length} agents · ▲${n("blocked")} ✔${n("done")} ◆${n("working")}`:"daemon offline";
  const by={};for(const a of S.agents)(by[a.cwd]??=[]).push(a);
  document.getElementById("fleet").innerHTML=Object.keys(by).sort().map(w=>`<div class="w">● ${esc(base(w))}</div>`+by[w].map(a=>
    `<div class="row"><span style="color:${COL[a.status]}">${GLYPH[a.status]||"·"}</span><span style="color:${KIND[a.kind]||"#9fb3c8"}">${esc(a.kind)}</span><span class="t">${esc(a.title||a.status)}${a.conflict?" ⚔":""}</span></div>`+
    (a.question?`<div class="q">${esc(a.question)}</div>`:"")).join("")).join("")||'<div class="sub">no agents</div>';
  const ms=S.missions.filter(m=>m.state!=="done").concat(S.missions.filter(m=>m.state==="done").slice(-5));
  document.getElementById("missions").innerHTML=ms.map(m=>`<div class="row"><span>${m.state==="done"?"★":m.state==="queued"?"○":"◆"}</span><span class="t" style="color:var(--text)">${esc(m.text)}</span><span>${esc(base(m.cwd))}</span></div>`).join("")||"none";
}
const cv=document.getElementById("c"),g=cv.getContext("2d");let stars=[];
function size(){const r=devicePixelRatio||1;cv.width=cv.clientWidth*r;cv.height=cv.clientHeight*r;g.setTransform(r,0,0,r,0,0);
  stars=Array.from({length:220},(_,i)=>[hash("x"+i)*cv.clientWidth,hash("y"+i)*cv.clientHeight,hash("b"+i)])}
addEventListener("resize",size);size();
function frame(t){t/=1000;const W=cv.clientWidth,H=cv.clientHeight;g.fillStyle="#070a14";g.fillRect(0,0,W,H);
  for(const[x,y,b]of stars){g.fillStyle=`rgba(180,200,255,${.15+.5*b*(.6+.4*Math.sin(t*2+b*9))})`;g.fillRect(x,y,1.3,1.3)}
  const sys={};for(const a of S.agents)((sys[parent(a.cwd)]??={})[a.cwd]??=[]).push(a);
  const keys=Object.keys(sys).sort(),n=keys.length,cx=W/2,cy=H/2;
  // scale the galaxy to the window: systems spread out, orbits grow with it
  const zoom=Math.max(.8,Math.min(W,H)/560),sp=Math.min(W,H)*(n>1?.36:0);
  // systems on a golden spiral, then the whole thing centred in the window
  const at=keys.map((k,i)=>{const ang=i*2.39996+.6,rr=sp*Math.sqrt((i+.6)/Math.max(n,1));return[Math.cos(ang)*rr,Math.sin(ang)*rr*.7]});
  const mx=at.length?(Math.min(...at.map(p=>p[0]))+Math.max(...at.map(p=>p[0])))/2:0,my=at.length?(Math.min(...at.map(p=>p[1]))+Math.max(...at.map(p=>p[1])))/2:0;
  keys.forEach((k,i)=>{
    const sx=cx+at[i][0]-mx,sy=cy+at[i][1]-my,pl=Object.keys(sys[k]).sort();
    const grd=g.createRadialGradient(sx,sy,0,sx,sy,28);grd.addColorStop(0,"#fff6d8");grd.addColorStop(.25,"#ffd27a");grd.addColorStop(1,"rgba(255,180,90,0)");
    g.fillStyle=grd;g.beginPath();g.arc(sx,sy,28,0,7);g.fill();
    g.fillStyle="#c9a86a";g.font="11px ui-monospace,monospace";g.textAlign="center";g.fillText(base(k).toUpperCase(),sx,sy-34);
    pl.forEach((p,j)=>{const orb=(46+34*j)*zoom,a0=hash(p)*6.283+t*.05/(1+.4*j);
      g.strokeStyle="rgba(60,110,140,.25)";g.beginPath();g.ellipse(sx,sy,orb,orb*.55,0,0,7);g.stroke();
      const px=sx+Math.cos(a0)*orb,py=sy+Math.sin(a0)*orb*.55,ags=sys[keys[i]][p],r=(8+2*Math.min(ags.length,6))*Math.min(zoom,1.6);
      const hue=hash(p+"h")*360,pg=g.createRadialGradient(px-r*.4,py-r*.4,1,px,py,r);pg.addColorStop(0,`hsl(${hue},60%,65%)`);pg.addColorStop(1,`hsl(${hue},55%,18%)`);
      g.fillStyle=pg;g.beginPath();g.arc(px,py,r,0,7);g.fill();
      if(ags.some(a=>a.status==="blocked")){g.strokeStyle=`rgba(255,90,74,${.4+.4*Math.abs(Math.sin(t*4))})`;g.lineWidth=2;g.beginPath();g.arc(px,py,r+5,0,7);g.stroke();g.lineWidth=1}
      ags.forEach((a,q)=>{const aa=t*(.5+hash(a.id+"s")*.3)+q*6.283/ags.length,o=r+9+(q%2)*4;
        const x=px+Math.cos(aa)*o,y=py+Math.sin(aa)*o*.55,c=COL[a.status]||"#888";
        g.fillStyle=c;g.shadowColor=c;g.shadowBlur=a.status==="working"||a.status==="blocked"?10:4;g.beginPath();g.arc(x,y,2.6,0,7);g.fill();g.shadowBlur=0});
      g.fillStyle="#cfd8e6";g.fillText(base(p),px,py+r+16)})});
  if(!S.agents.length){g.fillStyle="#6b7a90";g.textAlign="center";g.fillText(S.online?"the galaxy is uncharted":"daemon offline",W/2,H/2)}
  requestAnimationFrame(frame)}
poll();requestAnimationFrame(frame);
</script></body></html>
"##;
