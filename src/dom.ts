// An element of index.html by its id.
export const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
