"use client";
import { useState } from "react";
import { usePathname,useRouter } from "next/navigation";
import { useQuery,useQueryClient } from "@tanstack/react-query";
import { Plus,Plug,Monitor,Globe,RefreshCw } from "lucide-react";
import { Page,Grid,Surface,Stack,Cluster } from "@/components/foundation/layout/layout";
import { Button,PageHead,Status,ErrorBox,Empty,Loading,Input } from "./ui";
import { ConnectionDetail,CreateConnection } from "./Connections";
import { DirectoryPage } from "./Directory";
import { HostsPage,ActivityPage,SettingsPage,HelpPage } from "./Pages";
import { api,message,type DirectoryEntry } from "./lib/api";
import { parseRoute,routePath } from "./lib/routing";
import { AccountAccess } from "./account-access";
export function Workspace(){
 const route=parseRoute(usePathname());const router=useRouter();const cache=useQueryClient();
 const [create,setCreate]=useState(false);const[entry,setEntry]=useState<DirectoryEntry|null>(null);const[notice,setNotice]=useState("");const[search,setSearch]=useState("");
 const connections=useQuery({queryKey:["connections"],queryFn:api.connections,enabled:route.page==="connections"&&!route.connectionId});
 const detail=useQuery({queryKey:["connection",route.connectionId],queryFn:()=>api.connection(route.connectionId!),enabled:!!route.connectionId});
 const discovery=useQuery({queryKey:["discovery"],queryFn:api.discovery});
 const me=useQuery({queryKey:["me"],queryFn:api.me});
 const start=()=>{setEntry(null);setCreate(true);};
 return <Page><div className="mcport-product"><Stack gap={6}>
 {notice&&<p role="status" className="notice">{notice}</p>}
 {route.page==="connections"&&route.connectionId?(detail.isPending?<Loading/>:detail.error?<ErrorBox error={message(detail.error)} onRetry={()=>void detail.refetch()}/>:detail.data&&<ConnectionDetail connection={detail.data} tab={route.tab??"tools"} onTabChange={tab=>router.push(routePath({...route,tab}))} onBack={()=>router.push("/connections")} onUpdate={c=>{cache.setQueryData(["connection",c.id],c);void cache.invalidateQueries({queryKey:["connections"]});}} notify={setNotice}/>):route.page==="connections"?<>
  <PageHead title="Your connections" description="The tools you use, with the access you choose." action={<Button onClick={start}><Plus size={16}/>New connection</Button>}/>
  <Cluster><Input label="Search connections" value={search} onChange={e=>setSearch(e.target.value)} placeholder="Name, description or owner"/><Button variant="secondary" onClick={()=>connections.refetch()} loading={connections.isFetching}><RefreshCw size={16}/>Refresh</Button></Cluster>
  {connections.error&&<ErrorBox error={message(connections.error)} onRetry={()=>void connections.refetch()}/>}
  {connections.isPending?<Loading/>:!connections.data?.length&&!connections.error?<Empty title="Make your first connection" description="Find a server in the directory, or start with your own endpoint." icon={<Plug size={26}/>} action={<Cluster><Button onClick={()=>router.push("/directory")}>Browse directory</Button><Button variant="secondary" onClick={start}>Custom connection</Button></Cluster>}/>:<Grid min={280}>{connections.data?.filter(c=>(c.name+" "+c.description+" "+c.owner.id).toLowerCase().includes(search.toLowerCase())).map(c=><Surface key={c.id}><Stack><Cluster justify="between">{c.host_id?<Monitor size={20}/>:<Globe size={20}/>}<Status status={c.status}/></Cluster><h2 style={{fontSize:"var(--text-lg)"}}>{c.name}</h2><p>{c.description||"MCP tools and resources"}</p><small>{c.owner.id||c.owner.uuid} · {c.visibility==="circle"?"You and your Silicons":"Invite only"}</small><Button variant="secondary" onClick={()=>router.push("/connections/"+encodeURIComponent(c.id))}>Open connection</Button></Stack></Surface>)}</Grid>}
 </>:null}
 {route.page==="directory"&&<DirectoryPage onUse={e=>{setEntry(e);setCreate(true);}} onCustom={start} notify={setNotice}/>}
 {route.page==="hosts"&&<HostsPage/>}
 {route.page==="activity"&&<ActivityPage callId={route.callId} onSelect={callId=>router.push(routePath({page:"activity",callId}))}/>}
 {route.page==="settings"&&<>{me.isPending?<Loading/>:me.error?<ErrorBox error={message(me.error)}/>:me.data&&<SettingsPage session={me.data} discovery={discovery.data??null} notify={setNotice}/>}<AccountAccess/></>}
 {route.page==="help"&&<HelpPage discovery={discovery.data??null}/>}
 <CreateConnection open={create} onClose={()=>setCreate(false)} entry={entry} onCreated={c=>{setCreate(false);void cache.invalidateQueries({queryKey:["connections"]});router.push("/connections/"+encodeURIComponent(c.id));}}/>
 </Stack></div></Page>;
}
