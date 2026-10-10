import { expect,test,type BrowserContext,type Page } from "@playwright/test";
import { accountsSession,runAccounts,signIn,watchErrors } from "./support";
import { mkdirSync } from "node:fs";
test.describe.configure({mode:"serial"});
let context:BrowserContext,page:Page,errors:string[],connectionPath="";
test.beforeAll(async({browser})=>{context=await accountsSession(browser,"carbon");page=await context.newPage();errors=watchErrors(page);await signIn(page,runAccounts().carbon.email,"/connections");mkdirSync("../.mig/screens",{recursive:true});});
test.afterAll(async()=>{await context?.close();});
async function shot(name:string){await page.evaluate(()=>{(document.activeElement as HTMLElement)?.blur();window.scrollTo(0,0);});await page.waitForTimeout(250);await page.screenshot({path:"../.mig/screens/"+name+".png",fullPage:true,caret:"initial"});}
test("creates a connection, discovers paginated tools and executes a structured call",async()=>{
 await page.getByRole("button",{name:"New connection",exact:true}).click();
 const dialog=page.getByRole("dialog");await dialog.getByRole("textbox",{name:"Connection name",exact:true}).fill("browser-tools");
 await dialog.getByRole("button",{name:"Continue",exact:true}).click();
 await dialog.getByRole("textbox",{name:"MCP endpoint URL"}).fill("http://127.0.0.1:4242/mcp/public");
 await dialog.getByRole("button",{name:"Continue",exact:true}).click();
 await dialog.getByRole("button",{name:"Create connection",exact:true}).click();
 await expect(page.getByRole("heading",{name:"browser-tools",exact:true})).toBeVisible();
 connectionPath=new URL(page.url()).pathname;
 await expect(page.getByRole("button",{name:/nested Preserve nested/})).toBeVisible();
 await page.getByRole("button",{name:/echo Return the message/}).click();
 await page.getByRole("textbox",{name:"Tool arguments",exact:true}).fill(JSON.stringify({message:"A real browser tool call"}));
 await page.getByRole("button",{name:"Run tool",exact:true}).click();
 await expect(page.getByRole("heading",{name:"Structured output",exact:true})).toBeVisible();
 await expect(page.locator(".result-panel")).toContainText("A real browser tool call");
 await shot("mcport-tools-desktop");
});
test("tool policy switches and account grants persist",async()=>{
 const toggle=page.getByRole("switch",{name:"Disable echo",exact:true});await toggle.click();await expect(page.getByRole("switch",{name:"Enable echo",exact:true})).toBeVisible();await page.reload();await expect(page.getByRole("switch",{name:"Enable echo",exact:true})).toBeVisible();await page.getByRole("switch",{name:"Enable echo",exact:true}).click();
 await page.goto(connectionPath+"/access");
 await page.getByRole("textbox").filter({hasNot:page.locator("[type=password]")}).first().fill(runAccounts().friend.id);
 await page.getByRole("button",{name:"Grant access",exact:true}).click();
 await expect(page.locator(".access-list")).toContainText(runAccounts().friend.id);
 await shot("mcport-connection-access");
});
test("reads resources and prompts, then finds its call in activity",async()=>{
 await page.goto(connectionPath+"/resources");
 await page.getByRole("button",{name:"List resources",exact:true}).click();await expect(page.getByText("Full JSON result",{exact:true})).toBeVisible();
 await page.getByRole("textbox",{name:"Resource URI"}).fill("fixture://readme");await page.getByRole("button",{name:"Read resource",exact:true}).click();await expect(page.locator(".mcp-result")).toContainText("Fixture resource");
 await page.getByRole("combobox",{name:"Capability",exact:true}).click();await page.getByRole("option",{name:"Prompts",exact:true}).click();
 await page.getByRole("button",{name:"List prompts",exact:true}).click();await page.getByRole("textbox",{name:"Prompt name"}).fill("summarize");await page.getByRole("textbox",{name:"Prompt arguments (JSON)"}).fill('{"text":"Useful browser proof"}');await page.getByRole("button",{name:"Get prompt",exact:true}).click();await expect(page.locator(".mcp-result")).toContainText("Useful browser proof");
 await page.goto("/activity");await expect(page.getByText("browser-tools",{exact:true}).first()).toBeVisible();await shot("mcport-activity-desktop");
});
test("directory is searchable and personal entries can be created and shared",async()=>{
 await page.goto("/directory");await expect(page.getByRole("heading",{level:1})).toBeVisible();await page.getByRole("button",{name:"Add personal entry",exact:true}).click();
 const dialog=page.getByRole("dialog");await dialog.getByRole("textbox",{name:"Name",exact:true}).fill("Browser reference");await dialog.getByRole("textbox",{name:"Description",exact:true}).fill("A reusable MCP server reference.");await dialog.getByRole("textbox",{name:"Category",exact:true}).fill("Testing");await dialog.getByRole("button",{name:/Add entry|Save entry|Create entry/,exact:false}).click();
 const card=page.locator("article").filter({has:page.getByRole("heading",{name:"Browser reference",exact:true})});await expect(card).toBeVisible();
 await card.getByRole("button",{name:"Share entry",exact:true}).click();const share=page.getByRole("dialog");await share.getByRole("textbox",{name:"Carbon or Silicon ID"}).fill(runAccounts().friend.id);await share.getByRole("button",{name:"Share entry",exact:true}).click();await expect(share).toContainText(runAccounts().friend.id);await page.keyboard.press("Escape");await shot("mcport-directory-desktop");
});
test("mobile dark pages fit without client errors",async()=>{
 await page.setViewportSize({width:390,height:844});await page.emulateMedia({colorScheme:"dark",reducedMotion:"reduce"});
 for(const path of ["/connections",connectionPath,"/settings"]){await page.goto(path);await expect(page.getByRole("heading",{level:1})).toBeVisible();await page.waitForLoadState("networkidle");expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+2)).toBe(true);await shot("mcport-"+(path==="/settings"?"settings":path==="/connections"?"connections":"tools")+"-phone-dark");}
 expect(errors).toEqual([]);
});
