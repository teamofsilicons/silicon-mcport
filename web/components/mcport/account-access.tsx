"use client";
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api as transport } from "@/lib/client/api";
import { Section,Stack,Cluster } from "@/components/foundation/layout/layout";
import { Button,Input,ErrorBox } from "./ui";
import { message,type AccountRef } from "./lib/api";
import { HoldToConfirm } from "@/components/arc/hold-to-confirm/hold-to-confirm";
type Allowance={account:AccountRef;silicon:AccountRef;created_at:number};
export function AccountAccess(){const[silicon,setSilicon]=useState("");const[selector,setSelector]=useState("");const[target,setTarget]=useState("");const[error,setError]=useState("");const[busy,setBusy]=useState(false);
 const list=useQuery({queryKey:["allow",silicon],queryFn:()=>transport.get<{data:Allowance[]}>("/api/v1/allow",{query:silicon?{silicon}:{}}),enabled:!!silicon});
 async function change(remove?:string){setBusy(true);setError("");try{if(remove)await transport.delete("/api/v1/allow/"+encodeURIComponent(remove),{query:{silicon}});else await transport.post("/api/v1/allow",{silicon,account:target});setTarget("");await list.refetch();}catch(e){setError(message(e));}finally{setBusy(false);}}
 return <Section title="Who can share with your Silicons" description="Outside your circle, a Silicon accepts shares only from accounts it allows."><Stack><form onSubmit={e=>{e.preventDefault();setSilicon(selector.trim());}}><Cluster><Input label="Silicon ID" value={selector} onChange={e=>setSelector(e.target.value)} placeholder="si:scout" required/><Button type="submit" variant="secondary">View allowed accounts</Button></Cluster></form>{(error||list.error)&&<ErrorBox error={error||message(list.error)}/>} {!!silicon&&<><form onSubmit={e=>{e.preventDefault();void change();}}><Cluster><Input label="Carbon or Silicon ID" value={target} onChange={e=>setTarget(e.target.value)} placeholder="c:ada or si:scout" required/><Button type="submit" loading={busy} disabled={busy||!target.trim()}>Allow account</Button></Cluster></form>{list.data?.data.length===0&&<p>No outside accounts allowed.</p>}{list.data?.data.map(a=><Cluster key={a.account.uuid} justify="between"><span>{a.account.id||a.account.uuid}</span><HoldToConfirm label={"Hold to remove "+(a.account.id||a.account.uuid)} tone="danger" disabled={busy} onConfirm={()=>void change(a.account.uuid)}/></Cluster>)}</>}</Stack></Section>;
}
