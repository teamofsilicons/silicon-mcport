import { Plug, BookOpen, Monitor, Activity, Settings2, ShieldCheck, TerminalSquare, Bot, Wrench, Share2 } from "lucide-react";
import type { AppConfig } from "./app-config.types";
export const appConfig:AppConfig={
 appId:"mcport",name:"MCPort",brandPrefix:"Silicon",tagline:"One place for your MCP connections.",
 description:"Connect cloud and local MCP servers, choose who can use their tools, and inspect every result. Carbons and Silicons share the same connections through Silicon Accounts.",
 mark:{paths:["M7 2v5","M17 2v5","M5 7h14v4a7 7 0 0 1-14 0V7z","M12 18v4"]},
 cli:{command:"mcport"},home:"/connections",
 nav:[{href:"/connections",label:"Connections",icon:Plug},{href:"/directory",label:"Directory",icon:BookOpen},{href:"/hosts",label:"Hosts",icon:Monitor},{href:"/activity",label:"Activity",icon:Activity},{href:"/settings",label:"Settings",icon:Settings2},{href:"/help",label:"Help",icon:BookOpen}],
 links:{docs:"https://github.com/teamofsilicons/silicon-mcport/tree/main/docs",source:"https://github.com/teamofsilicons/silicon-mcport",store:"https://apps.teamofsilicons.com/apps/mcport"},
 signIn:{scopes:["email"]},
 landing:{headline:"Your tools. Connected.",lede:"Bring your MCP servers together. Discover tools, connect your accounts, and give the right people and Silicons access.",forCarbons:[{icon:Plug,title:"Cloud or close to home",text:"Connect an HTTPS endpoint or an MCP running on a machine you control."},{icon:Wrench,title:"Try the tools",text:"Inspect schemas, run calls, read resources and see structured results in one workspace."},{icon:Share2,title:"Share with exact accounts",text:"Invite a Carbon or Silicon, or make a connection available to you and the Silicons you look after."},{icon:ShieldCheck,title:"Keep control",text:"Choose tool permissions and provider authentication separately for every connection."}],forSilicons:[{icon:TerminalSquare,title:"Ready from the command line",text:"Install with Silicon Apps, sign in with Accounts, and use the same connections from your tools."},{icon:Bot,title:"Useful work, visible results",text:"Review activity, cancel pending work and download the files your tools return."}]}
};
export type { AppConfig, NavItem } from "./app-config.types";
