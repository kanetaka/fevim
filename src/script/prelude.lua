-- fevim Lua prelude: the `vim` API and the Vim script runtime (`vim._vs`).
-- `vim._core` (C) is provided by Rust.

local C = vim._core
local unpack = table.unpack

-------------------------------------------------------------------------------
-- Vim values
-------------------------------------------------------------------------------
local LIST_MT = { __name = 'vim.list' }
local DICT_MT = { __name = 'vim.dict' }
local FUNCREF_MT = { __name = 'vim.funcref' }
C.set_metatables(LIST_MT, DICT_MT)

local V = {}
vim._vs = V
V.LIST_MT, V.DICT_MT, V.FUNCREF_MT = LIST_MT, DICT_MT, FUNCREF_MT

function V.list(t) return setmetatable(t or {}, LIST_MT) end
function V.dict(t) return setmetatable(t or {}, DICT_MT) end

local function is_list(t)
  if type(t) ~= 'table' then return false end
  local mt = getmetatable(t)
  if mt == LIST_MT then return true end
  if mt == DICT_MT or mt == FUNCREF_MT then return false end
  return t[1] ~= nil or next(t) == nil
end
local function is_dict(t)
  if type(t) ~= 'table' then return false end
  local mt = getmetatable(t)
  if mt == DICT_MT then return true end
  if mt == LIST_MT or mt == FUNCREF_MT then return false end
  return next(t) ~= nil and t[1] == nil
end
V.is_list, V.is_dict = is_list, is_dict

vim.NIL = vim.NIL or setmetatable({}, { __name = 'vim.NIL', __tostring = function() return 'v:null' end })
function vim.empty_dict() return V.dict({}) end

-- string -> number like Vim's str2nr (leading digits)
local function str2num(s)
  s = s:match('^%s*(.-)$')
  local sign, rest = s:match('^([-+]?)(.*)$')
  local n
  local hex = rest:match('^0[xX](%x+)')
  if hex then
    n = tonumber(hex, 16)
  else
    local bin = rest:match('^0[bB]([01]+)')
    if bin then n = tonumber(bin, 2) else n = tonumber(rest:match('^%d+') or '0') end
  end
  n = math.tointeger(n) or 0
  if sign == '-' then n = -n end
  return n
end

function V.num(x)
  local t = type(x)
  if t == 'number' then return x end
  if t == 'string' then return str2num(x) end
  if t == 'boolean' then return x and 1 or 0 end
  if x == nil or x == vim.NIL then return 0 end
  error('E745: Using a ' .. (is_list(x) and 'List' or 'Dictionary') .. ' as a Number', 0)
end

local function fmt_float(f)
  if f == math.huge then return 'inf' end
  if f == -math.huge then return '-inf' end
  if f ~= f then return 'nan' end
  local s = string.format('%g', f)
  if not s:find('[%.eEn]') then s = s .. '.0' end
  return s
end

function V.str(x)
  local t = type(x)
  if t == 'string' then return x end
  if t == 'number' then
    if math.type(x) == 'integer' then return string.format('%d', x) end
    return fmt_float(x)
  end
  if t == 'boolean' then return x and '1' or '0' end
  if x == nil then return '' end
  if x == vim.NIL then return 'v:null' end
  if t == 'table' and getmetatable(x) == FUNCREF_MT then return x.name end
  if t == 'function' then return '<lambda>' end
  return V.repr(x)
end

-- string() representation
function V.repr(x, seen)
  local t = type(x)
  if t == 'string' then return "'" .. x:gsub("'", "''") .. "'" end
  if t == 'table' then
    if getmetatable(x) == FUNCREF_MT then return "function('" .. x.name .. "')" end
    seen = seen or {}
    if seen[x] then return is_list(x) and '[...]' or '{...}' end
    seen[x] = true
    local parts = {}
    if is_list(x) then
      for _, v in ipairs(x) do parts[#parts + 1] = V.repr(v, seen) end
      seen[x] = nil
      return '[' .. table.concat(parts, ', ') .. ']'
    end
    local keys = {}
    for k in pairs(x) do keys[#keys + 1] = tostring(k) end
    table.sort(keys)
    for _, k in ipairs(keys) do parts[#parts + 1] = "'" .. k .. "': " .. V.repr(x[k], seen) end
    seen[x] = nil
    return '{' .. table.concat(parts, ', ') .. '}'
  end
  if t == 'function' then return "function('<lambda>')" end
  if t == 'boolean' then return x and 'v:true' or 'v:false' end
  if x == nil or x == vim.NIL then return 'v:null' end
  return V.str(x)
end

function V.truthy(x)
  local t = type(x)
  if t == 'boolean' then return x end
  if t == 'number' then return x ~= 0 end
  if t == 'string' then return str2num(x) ~= 0 end
  if x == nil or x == vim.NIL then return false end
  if t == 'function' then return true end
  if t == 'table' then
    if getmetatable(x) == FUNCREF_MT then return true end
    error('E745: Using a ' .. (is_list(x) and 'List' or 'Dictionary') .. ' as a Number', 0)
  end
  return true
end

local function both_int(a, b) return math.type(a) == 'integer' and math.type(b) == 'integer' end

function V.add(a, b)
  if type(a) == 'table' and type(b) == 'table' then
    local r = V.list({})
    for _, v in ipairs(a) do r[#r + 1] = v end
    for _, v in ipairs(b) do r[#r + 1] = v end
    return r
  end
  return V.num(a) + V.num(b)
end
function V.sub(a, b) return V.num(a) - V.num(b) end
function V.mul(a, b) return V.num(a) * V.num(b) end
function V.div(a, b)
  a, b = V.num(a), V.num(b)
  if both_int(a, b) then
    if b == 0 then
      if a == 0 then return math.mininteger end
      return a > 0 and math.maxinteger or -math.maxinteger
    end
    local q = a // b
    if q < 0 and q * b ~= a then q = q + 1 end
    return q
  end
  return a / b
end
function V.mod(a, b)
  a, b = V.num(a), V.num(b)
  if both_int(a, b) then
    if b == 0 then return 0 end
    return math.fmod(a, b)
  end
  return math.fmod(a, b)
end
function V.neg(a) return -V.num(a) end

function V.concat(...)
  local n = select('#', ...)
  local parts = {}
  for i = 1, n do
    local v = select(i, ...)
    if type(v) == 'table' and getmetatable(v) ~= FUNCREF_MT then
      error('E730: Using a ' .. (is_list(v) and 'List' or 'Dictionary') .. ' as a String', 0)
    end
    parts[i] = V.str(v)
  end
  return table.concat(parts)
end

local function deep_eq(a, b, ic)
  if type(a) ~= type(b) then return false end
  if type(a) ~= 'table' then
    if type(a) == 'string' and ic then return a:lower() == b:lower() end
    return a == b
  end
  if getmetatable(a) == FUNCREF_MT or getmetatable(b) == FUNCREF_MT then
    return getmetatable(a) == getmetatable(b) and a.name == b.name
  end
  for k, v in pairs(a) do if not deep_eq(v, b[k], ic) then return false end end
  for k in pairs(b) do if a[k] == nil then return false end end
  return true
end

local function prep(a, b, ic)
  local ta, tb = type(a), type(b)
  if ta == 'boolean' then a = a and 1 or 0; ta = 'number' end
  if tb == 'boolean' then b = b and 1 or 0; tb = 'number' end
  if a == nil then a = 0; ta = 'number' end
  if b == nil then b = 0; tb = 'number' end
  if ta == 'number' or tb == 'number' then
    if ta == 'table' or tb == 'table' then return nil end
    return V.num(a), V.num(b)
  end
  if ta == 'string' and tb == 'string' then
    if ic == nil then ic = C.getopt('ignorecase', 'g') end
    if ic then return a:lower(), b:lower() end
    return a, b
  end
  return nil
end

function V.eq(a, b, ic)
  if type(a) == 'table' or type(b) == 'table' then
    if ic == nil then ic = C.getopt('ignorecase', 'g') end
    return deep_eq(a, b, ic)
  end
  local x, y = prep(a, b, ic)
  return x == y
end
function V.ne(a, b, ic) return not V.eq(a, b, ic) end
local function ord(op)
  return function(a, b, ic)
    local x, y = prep(a, b, ic)
    if x == nil then error('E692: Invalid operation for List or Dictionary', 0) end
    if op == '<' then return x < y elseif op == '<=' then return x <= y
    elseif op == '>' then return x > y else return x >= y end
  end
end
V.lt, V.le, V.gt, V.ge = ord('<'), ord('<='), ord('>'), ord('>=')
function V.match(a, pat, ic) return C.regex_match(V.str(a), V.str(pat), ic) end
function V.nomatch(a, pat, ic) return not C.regex_match(V.str(a), V.str(pat), ic) end
function V.is(a, b, ic)
  if type(a) == 'table' or type(b) == 'table' then return rawequal(a, b) end
  return V.eq(a, b, ic == nil and false or ic)
end
function V.isnot(a, b, ic) return not V.is(a, b, ic) end

function V.tern(c, a, b) if c then return a else return b end end
function V.ternl(c, a, b) if c then return a() else return b() end end
function V.discard() end

-- indexing ------------------------------------------------------------------
local function list_idx(t, i)
  i = V.num(i)
  if i < 0 then i = #t + i end
  return i
end

function V.index(x, i)
  local t = type(x)
  if t == 'string' or t == 'number' then
    x = V.str(x)
    i = V.num(i)
    if i < 0 then return '' end
    return x:sub(i + 1, i + 1)
  end
  if t == 'table' then
    if is_list(x) then
      local k = list_idx(x, i)
      local v = x[k + 1]
      if v == nil then error('E684: List index out of range: ' .. V.str(i), 0) end
      return v
    end
    local v = x[V.str(i)]
    if v == nil then error('E716: Key not present in Dictionary: "' .. V.str(i) .. '"', 0) end
    return v
  end
  if x == nil then error('E121: Undefined variable', 0) end
  error('E909: Cannot index a special variable', 0)
end

function V.slice(x, a, b)
  if type(x) == 'table' then
    local n = #x
    a = V.num(a); if a < 0 then a = n + a end
    if a < 0 then a = 0 end
    if b == nil then b = n - 1 else b = V.num(b); if b < 0 then b = n + b end end
    local r = V.list({})
    for k = a, math.min(b, n - 1) do r[#r + 1] = x[k + 1] end
    return r
  end
  local s = V.str(x)
  local n = #s
  a = V.num(a); if a < 0 then a = n + a end
  if a < 0 then a = 0 end
  if b == nil then b = n - 1 else b = V.num(b); if b < 0 then b = n + b end end
  return s:sub(a + 1, b + 1)
end

function V.setindex(x, i, v)
  if type(x) ~= 'table' then error('E689: Can only index a List, Dictionary or Blob', 0) end
  if is_list(x) and next(x) ~= nil then
    local k = list_idx(x, i)
    if k < 0 or k >= #x then error('E684: List index out of range: ' .. V.str(i), 0) end
    x[k + 1] = v
  elseif getmetatable(x) == LIST_MT then
    error('E684: List index out of range: ' .. V.str(i), 0)
  else
    if getmetatable(x) == nil then setmetatable(x, DICT_MT) end
    x[V.str(i)] = v
  end
end

function V.remove(x, i)
  if type(x) ~= 'table' then return end
  if is_list(x) and getmetatable(x) ~= DICT_MT then
    table.remove(x, list_idx(x, i) + 1)
  else
    x[V.str(i)] = nil
  end
end

-- `a.b`: dictionary member, or string concatenation with variable b
function V.dot(x, key, fallback)
  if is_dict(x) or (type(x) == 'table' and getmetatable(x) == DICT_MT) then
    local v = x[key]
    if v == nil then error('E716: Key not present in Dictionary: "' .. key .. '"', 0) end
    return v
  end
  return V.concat(x, fallback())
end

function V.iter(x)
  if x == nil then return ipairs({}) end
  if type(x) == 'string' then
    local chars = {}
    for _, c in utf8.codes(x) do chars[#chars + 1] = utf8.char(c) end
    return ipairs(chars)
  end
  if type(x) ~= 'table' then error('E714: List required', 0) end
  if is_dict(x) and next(x) ~= nil then error('E714: List required', 0) end
  -- iterate over a copy so the loop body may modify the list
  local copy = {}
  for k, v in ipairs(x) do copy[k] = v end
  return ipairs(copy)
end

function V.getopt(name, scope) return C.getopt(name, scope or '') end

-------------------------------------------------------------------------------
-- functions
-------------------------------------------------------------------------------
V.funcs = {}       -- user defined functions by name
local L = {}        -- builtins implemented in Lua
V.lua_builtins = L

function V.func(name, params, flags, body)
  if body == nil then body, flags = flags, {} end
  flags = flags or {}
  local nparams = #params
  V.funcs[name] = function(...)
    local n = select('#', ...)
    if n < nparams then error('E119: Not enough arguments for function: ' .. name, 0) end
    if n > nparams and not flags.varargs then error('E118: Too many arguments for function: ' .. name, 0) end
    local a = { firstline = C.cursor_line(), lastline = C.cursor_line() }
    for i = 1, nparams do a[params[i]] = (select(i, ...)) end
    if flags.varargs then
      local extra = V.list({})
      for i = nparams + 1, n do extra[#extra + 1] = (select(i, ...)) end
      a['000'] = extra
      a['0'] = #extra
      for i, v in ipairs(extra) do a[tostring(i)] = v end
    else
      a['0'] = 0
      a['000'] = V.list({})
    end
    local r = body(a, {})
    if r == nil then return 0 end
    return r
  end
end

-- function used as a dict member
function V.lambda(params, varargs, body)
  return function(...)
    local a = {}
    for i, p in ipairs(params) do a[p] = (select(i, ...)) end
    if varargs then
      local extra = V.list({ select(#params + 1, ...) })
      a['000'], a['0'] = extra, #extra
    end
    local r = body(a, {})
    if r == nil then return 0 end
    return r
  end
end

local function resolve_func(name)
  local f = V.funcs[name]
  if f then return f end
  if name:sub(1, 2) == 'g:' then return V.funcs[name:sub(3)] end
  return nil
end

function V.funcref(S, name, arglist, dict)
  if type(name) == 'function' then return name end
  if type(name) == 'table' and getmetatable(name) == FUNCREF_MT then return name end
  name = V.str(name)
  if name:sub(1, 2) == 's:' then name = S .. name:sub(3)
  elseif name:sub(1, 5) == '<SID>' then name = S .. name:sub(6) end
  return setmetatable({ name = name, args = arglist, self = dict }, FUNCREF_MT)
end
FUNCREF_MT.__call = function(self, ...) return V.call(self, ...) end

function V.call(f, ...)
  local t = type(f)
  if t == 'function' then
    local r = f(...)
    if r == nil then return 0 end
    return r
  end
  if t == 'table' and getmetatable(f) == FUNCREF_MT then
    if f.args and #f.args > 0 then
      local all = {}
      for _, v in ipairs(f.args) do all[#all + 1] = v end
      for i = 1, select('#', ...) do all[#all + 1] = (select(i, ...)) end
      return V.call(f.name, unpack(all, 1, #all))
    end
    return V.call(f.name, ...)
  end
  if t == 'string' then
    local uf = resolve_func(f)
    if uf then return uf(...) end
    return vim.fn[f](...)
  end
  error('E117: Unknown function: ' .. V.str(f), 0)
end

-- errors / exceptions --------------------------------------------------------
V.FINISH = setmetatable({}, { __name = 'fevim.finish' })
V.FINALLY = setmetatable({}, { __name = 'fevim.finally' })

function V.errmsg(e)
  if type(e) == 'string' then return e end
  if type(e) == 'table' and e.msg then return e.msg end
  local s = tostring(e)
  s = s:gsub('^runtime error: ', '')
  s = s:gsub('\nstack traceback:.*$', '')
  return s
end

function V.ret(v) return { v } end
function V.finish() error(V.FINISH, 0) end
function V.throw(v) error({ msg = V.str(v), thrown = true }, 0) end
function V.error(_, msg) error(msg, 0) end

function V.try(body, ...)
  local clauses = { ... }
  local finally
  for i = 1, #clauses, 2 do
    if clauses[i] == V.FINALLY then finally = clauses[i + 1] end
  end
  local ok, r = pcall(body)
  local pending
  if not ok then
    if r == V.FINISH then pending = r
    else
      local msg = V.errmsg(r)
      local exc = msg
      if type(r) ~= 'table' or not r.thrown then
        -- Vim prefixes errors with the command: "Vim(call):E117: ..."
        if not msg:match('^Vim') then exc = 'Vim:' .. msg end
      end
      local handled = false
      for i = 1, #clauses, 2 do
        local pat = clauses[i]
        if pat ~= V.FINALLY and not handled then
          if pat == '.*' or C.regex_match(exc, pat, false) or C.regex_match(msg, pat, false) then
            handled = true
            vim.v.exception = exc
            local ok2, r2 = pcall(clauses[i + 1])
            if ok2 then r = r2 else pending = r2 end
          end
        end
      end
      if not handled then pending = r end
      if handled and pending == nil then ok = true end
    end
  end
  if finally then
    local ok3, r3 = pcall(finally)
    if not ok3 then pending = r3 end
    if ok3 and r3 ~= nil then return r3 end
  end
  if pending ~= nil then error(pending, 0) end
  if ok then return r end
end

-- `:silent` around script commands
function V.silent(bang, f)
  C.silent(true, bang)
  local ok, e = pcall(f)
  C.silent(false, bang)
  if not ok and not bang then error(e, 0) end
end

function V.execute(...)
  local parts = {}
  for i = 1, select('#', ...) do parts[#parts + 1] = V.str((select(i, ...))) end
  vim.cmd(table.concat(parts, ' '))
end

local function echo_str(...)
  local parts = {}
  for i = 1, select('#', ...) do
    local v = select(i, ...)
    if type(v) == 'table' and getmetatable(v) ~= FUNCREF_MT then parts[#parts + 1] = V.repr(v)
    else parts[#parts + 1] = V.str(v) end
  end
  return table.concat(parts, ' ')
end
function V.echo(...) C.echo(echo_str(...), nil, false) end
function V.echon(...) C.echo(echo_str(...), nil, false, true) end
function V.echomsg(...) C.echo(echo_str(...), nil, true) end
function V.echoerr(...) error(echo_str(...), 0) end

-- replace <SID> and s:func( in command strings with the script's <SNR> prefix
function V.sid(S, s)
  s = s:gsub('<SID>', S)
  s = s:gsub('%f[%w_:]s:([%w_#]+)%s*%(', function(n) return S .. n .. '(' end)
  return s
end

-- scripts --------------------------------------------------------------------
local scripts = {}
local next_sid = 1
local cmdline_vars = {}
V.scripts = scripts

function V.script(name)
  if name == nil then
    return '<SNR>0_', cmdline_vars, function(_, f) return f() end
  end
  local sc = scripts[name]
  if not sc then
    sc = { sid = next_sid, vars = {}, name = name }
    next_sid = next_sid + 1
    scripts[name] = sc
  end
  local at = function(lnum, f)
    local ok, e = pcall(f)
    if not ok then
      if e == V.FINISH then error(e, 0) end
      if V.last_error_script ~= name then
        C.echo('Error detected while processing ' .. name .. ':', 'ErrorMsg', true)
        V.last_error_script = name
      end
      C.echo(string.format('line %4d:', lnum), 'LineNr', true)
      C.echo(V.errmsg(e), 'ErrorMsg', true)
    end
  end
  return '<SNR>' .. sc.sid .. '_', sc.vars, at
end

-- call `f`, turning Lua error objects into plain strings (used by Rust)
function vim._pcall(f, ...)
  local r = table.pack(pcall(f, ...))
  if not r[1] then
    if r[2] == V.FINISH then return end
    error(V.errmsg(r[2]), 0)
  end
  return unpack(r, 2, r.n)
end

-- run a compiled chunk, treating :finish as a normal return
function V.run(f)
  local ok, e = pcall(f)
  if not ok and e ~= V.FINISH then error(e, 0) end
end

-- expression evaluation (cached)
local expr_cache = {}
function V.eval(s)
  s = V.str(s)
  local f = expr_cache[s]
  if not f then
    local code = C.transpile_expr(s)
    local err
    f, err = load(code, '=eval', 't')
    if not f then error(err, 0) end
    expr_cache[s] = f
  end
  return f()
end

-------------------------------------------------------------------------------
-- builtins implemented in Lua (lists, dicts, funcrefs, math)
-------------------------------------------------------------------------------
local function to_callable(f)
  if type(f) == 'string' then
    -- an expression using v:val / v:key
    local code = C.transpile_expr(f)
    local fn = assert(load(code, '=map', 't'))
    return function(k, v)
      vim.v.key, vim.v.val = k, v
      return fn()
    end
  end
  return function(k, v) return V.call(f, k, v) end
end

function L.len(x)
  if type(x) == 'table' then
    if is_list(x) then return #x end
    local n = 0
    for _ in pairs(x) do n = n + 1 end
    return n
  end
  return #V.str(x)
end
function L.empty(x)
  if x == nil or x == vim.NIL then return 1 end
  local t = type(x)
  if t == 'table' then return next(x) == nil and 1 or 0 end
  if t == 'string' then return x == '' and 1 or 0 end
  if t == 'number' then return x == 0 and 1 or 0 end
  if t == 'boolean' then return x and 0 or 1 end
  return 0
end
function L.type(x)
  local t = type(x)
  if t == 'number' then return math.type(x) == 'integer' and 0 or 5 end
  if t == 'string' then return 1 end
  if t == 'function' then return 2 end
  if t == 'boolean' then return 6 end
  if x == nil or x == vim.NIL then return 7 end
  if getmetatable(x) == FUNCREF_MT then return 2 end
  return is_list(x) and 3 or 4
end
function L.get(x, k, def)
  if type(x) ~= 'table' then return def or 0 end
  local v
  if is_list(x) then
    local i = list_idx(x, k)
    v = x[i + 1]
  else
    v = x[V.str(k)]
  end
  if v == nil then
    if def == nil then return 0 end
    return def
  end
  return v
end
function L.has_key(d, k) return (type(d) == 'table' and d[V.str(k)] ~= nil) and 1 or 0 end
function L.keys(d)
  local r = V.list({})
  for k in pairs(d) do r[#r + 1] = tostring(k) end
  table.sort(r)
  return r
end
function L.values(d)
  local r = V.list({})
  for _, k in ipairs(L.keys(d)) do r[#r + 1] = d[k] end
  return r
end
function L.items(d)
  local r = V.list({})
  for _, k in ipairs(L.keys(d)) do r[#r + 1] = V.list({ k, d[k] }) end
  return r
end
local function copy(x, deep, seen)
  if type(x) ~= 'table' or getmetatable(x) == FUNCREF_MT then return x end
  seen = seen or {}
  if seen[x] then return seen[x] end
  local r = setmetatable({}, getmetatable(x))
  seen[x] = r
  for k, v in pairs(x) do r[k] = deep and copy(v, true, seen) or v end
  return r
end
function L.copy(x) return copy(x, false) end
function L.deepcopy(x) return copy(x, true) end
function L.add(l, v)
  if type(l) ~= 'table' then error('E897: List or Blob required', 0) end
  l[#l + 1] = v
  return l
end
function L.insert(l, v, idx)
  idx = idx and V.num(idx) or 0
  if idx < 0 then idx = #l + idx end
  table.insert(l, idx + 1, v)
  return l
end
function L.remove(x, i, e)
  if is_dict(x) then
    local k = V.str(i)
    local v = x[k]
    if v == nil then error('E716: Key not present in Dictionary: "' .. k .. '"', 0) end
    x[k] = nil
    return v
  end
  local s = list_idx(x, i)
  if s < 0 or s >= #x then error('E684: List index out of range: ' .. V.str(i), 0) end
  if e == nil then return table.remove(x, s + 1) end
  local last = list_idx(x, e)
  local r = V.list({})
  for _ = s, last do r[#r + 1] = table.remove(x, s + 1) end
  return r
end
function L.extend(a, b, how)
  if is_list(a) and not is_dict(b) then
    local at = how and (list_idx(a, how) + 1) or (#a + 1)
    for i, v in ipairs(b) do table.insert(a, at + i - 1, v) end
    return a
  end
  for k, v in pairs(b) do
    if a[k] ~= nil and how == 'error' then error('E737: Key already exists: ' .. k, 0) end
    if a[k] == nil or how ~= 'keep' then a[k] = v end
  end
  return a
end
function L.extendnew(a, b, how) return L.extend(copy(a), b, how) end
function L.index(l, v, start, ic)
  start = start and V.num(start) or 0
  for i = start + 1, #l do if V.eq(l[i], v, V.truthy(ic or 0)) then return i - 1 end end
  return -1
end
function L.count(l, v, ic)
  local n = 0
  if type(l) == 'string' then
    local s, p = l, V.str(v)
    if p == '' then return 0 end
    local i = 1
    while true do
      local a, b = s:find(p, i, true)
      if not a then break end
      n = n + 1
      i = b + 1
    end
    return n
  end
  for _, x in pairs(l) do if V.eq(x, v, V.truthy(ic or 0)) then n = n + 1 end end
  return n
end
function L.join(l, sep)
  if sep == nil then sep = ' ' end
  local parts = {}
  for i, v in ipairs(l) do
    if type(v) == 'table' then parts[i] = V.repr(v) else parts[i] = V.str(v) end
  end
  return table.concat(parts, V.str(sep))
end
function L.reverse(l)
  if type(l) == 'string' then
    local chars = {}
    for _, c in utf8.codes(l) do table.insert(chars, 1, utf8.char(c)) end
    return table.concat(chars)
  end
  local n = #l
  for i = 1, n // 2 do l[i], l[n - i + 1] = l[n - i + 1], l[i] end
  return l
end
function L.sort(l, how)
  local cmp
  if how == nil or how == '' or how == 0 then
    cmp = function(a, b) return V.str(a) < V.str(b) end
  elseif how == 1 or how == 'i' then
    cmp = function(a, b) return V.str(a):lower() < V.str(b):lower() end
  elseif how == 'n' or how == 'N' then
    cmp = function(a, b) return V.num(a) < V.num(b) end
  elseif how == 'f' then
    cmp = function(a, b) return V.num(a) < V.num(b) end
  else
    cmp = function(a, b) return V.num(V.call(how, a, b)) < 0 end
  end
  -- stable sort
  local idx = {}
  for i, v in ipairs(l) do idx[i] = { v, i } end
  table.sort(idx, function(x, y)
    if cmp(x[1], y[1]) then return true end
    if cmp(y[1], x[1]) then return false end
    return x[2] < y[2]
  end)
  for i, p in ipairs(idx) do l[i] = p[1] end
  return l
end
function L.uniq(l)
  local i = 2
  while i <= #l do
    if V.eq(l[i], l[i - 1], false) then table.remove(l, i) else i = i + 1 end
  end
  return l
end
function L.map(x, f)
  local fn = to_callable(f)
  if is_list(x) then
    for i, v in ipairs(x) do x[i] = fn(i - 1, v) end
  elseif type(x) == 'string' then
    local out = {}
    local i = 0
    for _, c in utf8.codes(x) do out[#out + 1] = V.str(fn(i, utf8.char(c))); i = i + 1 end
    return table.concat(out)
  else
    for k, v in pairs(x) do x[k] = fn(k, v) end
  end
  return x
end
function L.mapnew(x, f) return L.map(copy(x), f) end
function L.filter(x, f)
  local fn = to_callable(f)
  if is_list(x) then
    local i, k = 1, 0
    while i <= #x do
      if V.truthy(fn(k, x[i])) then i = i + 1 else table.remove(x, i) end
      k = k + 1
    end
  else
    for k, v in pairs(x) do if not V.truthy(fn(k, v)) then x[k] = nil end end
  end
  return x
end
function L.foreach(x, f)
  local fn = to_callable(f)
  if is_list(x) then for i, v in ipairs(x) do fn(i - 1, v) end
  else for k, v in pairs(x) do fn(k, v) end end
  return x
end
function L.range(a, b, step)
  a = V.num(a)
  if b == nil then a, b = 0, a - 1 else b = V.num(b) end
  step = step and V.num(step) or 1
  if step == 0 then error('E726: Stride is zero', 0) end
  local r = V.list({})
  for i = a, b, step do r[#r + 1] = i end
  return r
end
function L.max(l)
  local m = 0
  local first = true
  for _, v in pairs(l) do
    v = V.num(v)
    if first or v > m then m = v end
    first = false
  end
  return m
end
function L.min(l)
  local m = 0
  local first = true
  for _, v in pairs(l) do
    v = V.num(v)
    if first or v < m then m = v end
    first = false
  end
  return m
end
function L.flatten(l, depth)
  depth = depth and V.num(depth) or math.huge
  local r = V.list({})
  local function go(x, d)
    for _, v in ipairs(x) do
      if type(v) == 'table' and is_list(v) and d > 0 then go(v, d - 1) else r[#r + 1] = v end
    end
  end
  go(l, depth)
  return r
end
function L.string(x) return V.repr(x) end
function L.eval(s) return V.eval(s) end
L['function'] = function(name, arglist, dict) return V.funcref('<SNR>0_', name, arglist, dict) end
L.funcref = L['function']
function L.call(f, args, dict) return V.call(f, unpack(args or {}, 1, #(args or {}))) end
function L.exists(what)
  what = V.str(what)
  local c = what:sub(1, 1)
  if c == '*' then
    local n = what:sub(2)
    if resolve_func(n) or L[n] or C.is_builtin(n) then return 1 end
    if n:find('#', 1, true) and C.autoload(n) and resolve_func(n) then return 1 end
    return 0
  end
  if c == '&' or c == '+' or c == ':' or c == '#' or c == '$' then
    return C.exists(what) and 1 or 0
  end
  -- variables
  local name = what:match('^[%w_:#]+')
  if not name then return 0 end
  local rest = what:sub(#name + 1)
  local scope, var = name:match('^([gbwtvsl]):(.*)$')
  local tbl
  if scope == 'g' or not scope then tbl = vim.g; var = var or name
  elseif scope == 'b' then tbl = vim.b
  elseif scope == 'w' then tbl = vim.w
  elseif scope == 't' then tbl = vim.t
  elseif scope == 'v' then tbl = vim.v
  else return 0 end
  local v = tbl[var]
  if v == nil then return 0 end
  -- exists('g:dict.key') / exists('g:list[0]')
  for key in rest:gmatch('%.([%w_]+)') do
    if type(v) ~= 'table' or v[key] == nil then return 0 end
    v = v[key]
  end
  return 1
end
function L.submatch(n)
  local m = vim.g.__submatch
  if type(m) ~= 'table' then return '' end
  return m[V.num(n) + 1] or ''
end
function L.abs(x) x = V.num(x); return x < 0 and -x or x end
function L.float2nr(x) return math.tointeger(x >= 0 and math.floor(x) or math.ceil(x)) or 0 end
function L.round(x) x = V.num(x) + 0.0; return x >= 0 and math.floor(x + 0.5) or -math.floor(-x + 0.5) end
function L.floor(x) return math.floor(V.num(x)) + 0.0 end
function L.ceil(x) return math.ceil(V.num(x)) + 0.0 end
function L.trunc(x) x = V.num(x) + 0.0; return x >= 0 and math.floor(x) or math.ceil(x) end
function L.sqrt(x) return math.sqrt(V.num(x)) end
function L.pow(a, b) return (V.num(a) + 0.0) ^ V.num(b) end
function L.fmod(a, b) return math.fmod(V.num(a) + 0.0, V.num(b)) end
function L.log(x) return math.log(V.num(x)) end
function L.log10(x) return math.log(V.num(x), 10) end
function L.exp(x) return math.exp(V.num(x)) end
function L.sin(x) return math.sin(V.num(x)) end
function L.cos(x) return math.cos(V.num(x)) end
function L.str2float(s) return tonumber(V.str(s):match('^%s*([-+]?%d*%.?%d+[eE]?[-+]?%d*)')) or 0.0 end
L['and'] = function(a, b) return V.num(a) & V.num(b) end
L['or'] = function(a, b) return V.num(a) | V.num(b) end
function L.xor(a, b) return V.num(a) ~ V.num(b) end
function L.invert(a) return ~V.num(a) end
function L.list2str(l)
  local out = {}
  for _, c in ipairs(l) do out[#out + 1] = utf8.char(V.num(c)) end
  return table.concat(out)
end
function L.str2list(s)
  local r = V.list({})
  for _, c in utf8.codes(V.str(s)) do r[#r + 1] = c end
  return r
end
function L.repeat_(x, n)
  if type(x) == 'table' then
    local r = V.list({})
    for _ = 1, V.num(n) do for _, v in ipairs(x) do r[#r + 1] = v end end
    return r
  end
  return C.fn('repeat', x, n)
end
L['repeat'] = L.repeat_
function L.system(cmd, input)
  if type(cmd) == 'table' then cmd = table.concat(cmd, ' ') end
  local out, code = C.system(V.str(cmd), input and (type(input) == 'table' and table.concat(input, '\n') or V.str(input)) or nil)
  vim.v.shell_error = code
  return out
end
function L.systemlist(cmd, input)
  local out = L.system(cmd, input)
  local r = V.list({})
  for line in (out .. '\n'):gmatch('(.-)\r?\n') do r[#r + 1] = line end
  if r[#r] == '' then r[#r] = nil end
  return r
end
function L.execute(cmd, silent)
  if type(cmd) == 'table' then cmd = table.concat(cmd, '\n') end
  return C.capture(V.str(cmd))
end
function L.getbufvar(buf, name, def)
  local b = vim._bufvars(C.bufnr(buf))
  if name == '' then return V.dict(b) end
  if name:sub(1, 1) == '&' then return C.getopt(name:sub(2), 'l') end
  local v = b[name]
  if v == nil then return def or '' end
  return v
end
function L.setbufvar(buf, name, v)
  if name:sub(1, 1) == '&' then C.setopt(name:sub(2), v, 'l', '='); return 0 end
  vim._bufvars(C.bufnr(buf))[name] = v
  return 0
end
function L.getwinvar(win, name, def)
  if name:sub(1, 1) == '&' then return C.getopt(name:sub(2), 'l') end
  local v = vim._winvars(C.winid(win))[name]
  if v == nil then return def or '' end
  return v
end
function L.setwinvar(win, name, v)
  if name:sub(1, 1) == '&' then C.setopt(name:sub(2), v, 'l', '='); return 0 end
  vim._winvars(C.winid(win))[name] = v
  return 0
end
function L.gettabvar(_, name, def) local v = vim.t[name]; if v == nil then return def or '' end return v end
function L.settabvar(_, name, v) vim.t[name] = v; return 0 end
function L.environ()
  return V.dict(C.environ())
end
function L.setenv(name, v) vim.env[name] = v; return 0 end
function L.timer_start(ms, f, opts)
  return vim.defer_fn(function() V.call(f, 0) end, V.num(ms))
end
function L.timer_stop(id) vim._timers[id] = nil; return 0 end
function L.json_encode(x) return vim.json.encode(x) end
function L.json_decode(s) return vim.json.decode(s) end
function L.luaeval(code, arg)
  local f = assert(load('return ' .. V.str(code), '=luaeval'))
  _G._A = arg
  return f()
end
function L.funcs() return V.funcs end

-------------------------------------------------------------------------------
-- vim.fn
-------------------------------------------------------------------------------
vim.fn = setmetatable({}, {
  __index = function(_, name)
    local uf = resolve_func(name)
    if uf then return uf end
    local lf = L[name]
    if lf then return lf end
    if C.is_builtin(name) then
      return function(...) return C.fn(name, ...) end
    end
    return function(...)
      local f = resolve_func(name)
      if not f and name:find('#', 1, true) and C.autoload(name) then f = resolve_func(name) end
      if f then return f(...) end
      error('E117: Unknown function: ' .. name, 0)
    end
  end,
})

function vim.call(name, ...) return vim.fn[name](...) end

-------------------------------------------------------------------------------
-- variables
-------------------------------------------------------------------------------
vim.g = vim.g or {}
vim.v = {
  ['true'] = true, ['false'] = false, count = 0, count1 = 1, version = 901, val = nil, key = nil,
  exception = '', throwpoint = '', errmsg = '', shell_error = 0, lang = 'C', progname = 'fevim',
  register = '"', hlsearch = 1, servername = '', searchforward = 1,
}
local bufvars, winvars, tabvars = {}, {}, {}
function vim._bufvars(id) local t = bufvars[id]; if not t then t = {}; bufvars[id] = t end return t end
function vim._winvars(id) local t = winvars[id]; if not t then t = {}; winvars[id] = t end return t end
local function scoped(getter, idfn)
  return setmetatable({}, {
    __index = function(_, k)
      if type(k) == 'number' then return getter(k == 0 and idfn() or k) end
      return getter(idfn())[k]
    end,
    __newindex = function(_, k, v) getter(idfn())[k] = v end,
  })
end
vim.b = scoped(vim._bufvars, C.curbuf)
vim.w = scoped(vim._winvars, C.curwin)
vim.t = scoped(function(id) local t = tabvars[id]; if not t then t = {}; tabvars[id] = t end return t end, C.curtab)

vim.env = setmetatable({}, {
  __index = function(_, k) return os.getenv(k) end,
  __newindex = function(_, k, v) C.setenv(k, v ~= nil and V.str(v) or nil) end,
})

-------------------------------------------------------------------------------
-- options
-------------------------------------------------------------------------------
local function opt_value_to_string(info, v)
  if type(v) == 'table' then
    if is_list(v) then
      local parts = {}
      for i, x in ipairs(v) do parts[i] = V.str(x) end
      return table.concat(parts, info and info.flaglist and '' or ',')
    end
    local parts = {}
    for k, x in pairs(v) do
      if x == true then parts[#parts + 1] = k
      elseif x ~= false then parts[#parts + 1] = k .. ':' .. V.str(x) end
    end
    table.sort(parts)
    return table.concat(parts, info and info.flaglist and '' or ',')
  end
  return v
end

local function opt_table(scope)
  return setmetatable({}, {
    __index = function(_, k) return C.getopt(k, scope) end,
    __newindex = function(_, k, v)
      local info = C.optinfo(k)
      C.setopt(k, opt_value_to_string(info, v), scope, '=')
    end,
  })
end
vim.o = opt_table('')
vim.go = opt_table('g')
vim.bo = opt_table('l')
vim.wo = opt_table('l')
vim.opt_value_to_string = opt_value_to_string

local Opt = {}
Opt.__index = Opt
function Opt:get()
  local v = C.getopt(self._name, self._scope)
  local info = C.optinfo(self._name)
  if type(v) ~= 'string' or not info then return v end
  if info.flaglist then
    local r = {}
    for c in v:gmatch('.') do r[c] = true end
    return r
  end
  if info.comma then
    local r = {}
    local is_map = v:find(':') ~= nil and self._name ~= 'guifont' and self._name ~= 'printfont' and self._name ~= 'backupdir' and self._name ~= 'directory' and self._name ~= 'undodir'
    for item in v:gmatch('[^,]+') do
      local k, x = item:match('^([^:]+):(.*)$')
      if is_map and k then r[k] = x else r[#r + 1] = item end
    end
    return r
  end
  return v
end
function Opt:append(v) C.setopt(self._name, opt_value_to_string(C.optinfo(self._name), v), self._scope, '+'); return self end
function Opt:prepend(v) C.setopt(self._name, opt_value_to_string(C.optinfo(self._name), v), self._scope, '^'); return self end
function Opt:remove(v)
  local info = C.optinfo(self._name)
  if type(v) == 'table' and is_list(v) and info and info.comma then
    for _, x in ipairs(v) do C.setopt(self._name, V.str(x), self._scope, '-') end
  else
    C.setopt(self._name, opt_value_to_string(info, v), self._scope, '-')
  end
  return self
end
Opt.__add = function(a, b) return { _op = '+', _val = b } end
Opt.__sub = function(a, b) return { _op = '-', _val = b } end
Opt.__pow = function(a, b) return { _op = '^', _val = b } end

local function opt_accessor(scope)
  return setmetatable({}, {
    __index = function(_, k)
      if not C.optinfo(k) then error('Unknown option: ' .. tostring(k), 2) end
      return setmetatable({ _name = k, _scope = scope }, Opt)
    end,
    __newindex = function(_, k, v)
      local info = C.optinfo(k)
      if not info then error('E518: Unknown option: ' .. tostring(k), 2) end
      if type(v) == 'table' and v._op then
        local o = setmetatable({ _name = k, _scope = scope }, Opt)
        if v._op == '+' then o:append(v._val) elseif v._op == '-' then o:remove(v._val) else o:prepend(v._val) end
        return
      end
      C.setopt(k, opt_value_to_string(info, v), scope == '' and 'b' or scope, '=')
    end,
  })
end
vim.opt = opt_accessor('')
vim.opt_local = opt_accessor('l')
vim.opt_global = opt_accessor('g')

-------------------------------------------------------------------------------
-- commands
-------------------------------------------------------------------------------
vim.cmd = setmetatable({}, {
  __call = function(_, s)
    if type(s) == 'table' then
      -- vim.cmd({ cmd = 'set', args = { 'nu' }, bang = true })
      local c = s.cmd .. (s.bang and '!' or '')
      if s.args then c = c .. ' ' .. table.concat(s.args, ' ') end
      return C.cmd(c)
    end
    return C.cmd(s)
  end,
  __index = function(_, name)
    return function(...)
      local args = { ... }
      local bang = ''
      if type(args[1]) == 'table' then
        local t = args[1]
        bang = t.bang and '!' or ''
        args = t.args or t
      end
      local parts = {}
      for i, a in ipairs(args) do parts[i] = V.str(a) end
      return C.cmd(name .. bang .. (#parts > 0 and (' ' .. table.concat(parts, ' ')) or ''))
    end
  end,
})

-------------------------------------------------------------------------------
-- function references for Rust
-------------------------------------------------------------------------------
local refs = {}
local next_ref = 1
vim._refs = refs
function vim._ref(f)
  local id = next_ref
  next_ref = next_ref + 1
  refs[id] = f
  return id
end
function vim._call_ref(id, ...)
  local f = refs[id]
  if not f then error('E117: Unknown function reference', 0) end
  return V.call(f, ...)
end

-------------------------------------------------------------------------------
-- keymaps
-------------------------------------------------------------------------------
vim.keymap = {}
local function bufopt(b)
  if b == nil or b == false then return nil end
  if b == true or b == 0 then return C.curbuf() end
  return b
end
function vim.keymap.set(modes, lhs, rhs, opts)
  opts = opts or {}
  if type(modes) == 'string' then modes = { modes } end
  local noremap = not opts.remap
  if opts.noremap ~= nil then noremap = opts.noremap end
  for _, m in ipairs(modes) do
    local fnid, rtext = nil, nil
    if type(rhs) == 'function' then fnid = vim._ref(rhs) else rtext = rhs end
    C.map(m, lhs, rtext, fnid, noremap, opts.silent and true or false, opts.expr and true or false, opts.nowait and true or false, bufopt(opts.buffer))
  end
end
function vim.keymap.del(modes, lhs, opts)
  opts = opts or {}
  if type(modes) == 'string' then modes = { modes } end
  for _, m in ipairs(modes) do C.unmap(m, lhs, bufopt(opts.buffer)) end
end

-------------------------------------------------------------------------------
-- vim.api
-------------------------------------------------------------------------------
local api = {}
vim.api = api
local augroup_names = {}

local function group_name(g)
  if g == nil then return nil end
  if type(g) == 'number' then return augroup_names[g] end
  return g
end

function api.nvim_create_augroup(name, opts)
  local clear = true
  if opts and opts.clear == false then clear = false end
  local id = C.augroup(name, clear)
  augroup_names[id] = name
  return id
end
function api.nvim_del_augroup_by_name(name) C.del_augroup(name) end
function api.nvim_del_augroup_by_id(id) if augroup_names[id] then C.del_augroup(augroup_names[id]) end end

local function as_list(x)
  if x == nil then return nil end
  if type(x) == 'table' then return x end
  return { x }
end

function api.nvim_create_autocmd(events, opts)
  opts = opts or {}
  local fnid
  if opts.callback then
    local cb = opts.callback
    fnid = vim._ref(function(ev)
      if type(cb) == 'string' then return V.call(cb) end
      local r = cb(ev)
      if r == true then return end
      return r
    end)
  end
  local buffer = opts.buffer
  if buffer == 0 or buffer == true then buffer = C.curbuf() end
  return C.autocmd(as_list(events), as_list(opts.pattern) or { '*' }, group_name(opts.group), opts.command, fnid, opts.once and true or false, buffer)
end
function api.nvim_del_autocmd(id) C.del_autocmd(id) end
function api.nvim_clear_autocmds(opts)
  opts = opts or {}
  local events = as_list(opts.event) or { false }
  for _, e in ipairs(events) do
    C.clear_autocmds(e or nil, opts.pattern and (as_list(opts.pattern)[1]) or nil, group_name(opts.group))
  end
end
function api.nvim_exec_autocmds(events, opts)
  opts = opts or {}
  for _, e in ipairs(as_list(events)) do C.cmd('doautocmd ' .. e .. ' ' .. (opts.pattern or '')) end
end

function api.nvim_create_user_command(name, cmd, opts)
  opts = opts or {}
  local fnid, text
  if type(cmd) == 'function' then fnid = vim._ref(cmd) else text = cmd end
  local nargs = opts.nargs ~= nil and tostring(opts.nargs) or '0'
  C.user_command(name, text, fnid, nargs, opts.bang and true or false, (opts.range or opts.count) and true or false, opts.bar and true or false)
end
function api.nvim_del_user_command(name) C.cmd('delcommand ' .. name) end
function api.nvim_buf_create_user_command(_, name, cmd, opts) api.nvim_create_user_command(name, cmd, opts) end

function api.nvim_set_hl(ns, name, val)
  val = val or {}
  local spec = {}
  local function col(c)
    if type(c) == 'number' then return string.format('#%06x', c) end
    return c
  end
  if val.link then
    C.set_hl(name, nil, val.link, val.default and true or false)
    return
  end
  if val.fg or val.foreground then spec.guifg = col(val.fg or val.foreground) end
  if val.bg or val.background then spec.guibg = col(val.bg or val.background) end
  if val.sp or val.special then spec.guisp = col(val.sp or val.special) end
  local attrs = {}
  for _, a in ipairs({ 'bold', 'italic', 'underline', 'undercurl', 'strikethrough', 'reverse', 'standout' }) do
    if val[a] then attrs[#attrs + 1] = a end
  end
  spec.gui = #attrs > 0 and table.concat(attrs, ',') or 'NONE'
  if val.ctermfg then spec.ctermfg = tostring(val.ctermfg) end
  if val.ctermbg then spec.ctermbg = tostring(val.ctermbg) end
  C.set_hl(name, spec, nil, val.default and true or false)
end
function api.nvim_get_hl(ns, opts)
  return C.get_hl(opts and opts.name or '')
end

function api.nvim_command(c) C.cmd(c) end
function api.nvim_exec2(src, opts)
  if opts and opts.output then return { output = C.capture(src) } end
  C.cmd(src)
  return {}
end
function api.nvim_exec(src, output)
  if output then return C.capture(src) end
  C.cmd(src)
  return ''
end
function api.nvim_eval(e) return V.eval(e) end
function api.nvim_call_function(name, args) return vim.fn[name](unpack(args or {})) end
function api.nvim_get_current_buf() return C.curbuf() end
function api.nvim_get_current_win() return C.curwin() end
function api.nvim_get_current_tabpage() return C.curtab() end
function api.nvim_set_current_buf(b) C.cmd('buffer ' .. b) end
function api.nvim_list_bufs() return C.buf_list() end
function api.nvim_list_wins() return C.win_list() end
function api.nvim_buf_is_valid(b) return C.buf_valid(b == 0 and C.curbuf() or b) end
function api.nvim_buf_get_name(b) return C.buf_name(b == 0 and C.curbuf() or b) end
function api.nvim_buf_set_name(b, name) C.cmd('file ' .. name) end
function api.nvim_buf_line_count(b) return C.buf_line_count(b == 0 and C.curbuf() or b) end
function api.nvim_buf_get_lines(b, s, e, strict) return C.buf_get_lines(b == 0 and C.curbuf() or b, s, e) end
function api.nvim_buf_set_lines(b, s, e, strict, lines) C.buf_set_lines(b == 0 and C.curbuf() or b, s, e, lines) end
function api.nvim_get_current_line() return vim.fn.getline('.') end
function api.nvim_set_current_line(l) vim.fn.setline('.', l) end
function api.nvim_win_get_buf(w) return C.win_buf(w == 0 and C.curwin() or w) end
function api.nvim_win_get_cursor(w)
  local l, c = C.get_cursor(w == 0 and C.curwin() or w)
  return { l, c }
end
function api.nvim_win_set_cursor(w, pos) C.set_cursor(w == 0 and C.curwin() or w, pos[1], pos[2]) end
function api.nvim_win_get_width(w) return vim.fn.winwidth(w == 0 and 0 or w) end
function api.nvim_win_get_height(w) return vim.fn.winheight(w == 0 and 0 or w) end
function api.nvim_feedkeys(keys, mode, escape_ks) vim.fn.feedkeys(keys, mode or '') end
function api.nvim_input(keys) vim.fn.feedkeys(vim.keycode(keys), 't'); return #keys end
function api.nvim_replace_termcodes(s, from_part, do_lt, special) return C.keycode(s) end
function api.nvim_echo(chunks, history, opts)
  for _, ch in ipairs(chunks) do C.echo(ch[1], ch[2], history and true or false, true) end
end
function api.nvim_err_writeln(msg) C.echo(msg, 'ErrorMsg', true) end
function api.nvim_out_write(msg) C.echo((msg:gsub('\n$', '')), nil, false) end
function api.nvim_err_write(msg) C.echo((msg:gsub('\n$', '')), 'ErrorMsg', true) end
function api.nvim_get_option_value(name, opts)
  local scope = (opts and opts.scope == 'global') and 'g' or ((opts and (opts.scope == 'local' or opts.buf or opts.win)) and 'l' or '')
  return C.getopt(name, scope)
end
function api.nvim_set_option_value(name, v, opts)
  local scope = (opts and opts.scope == 'global') and 'g' or ((opts and (opts.scope == 'local' or opts.buf or opts.win)) and 'l' or 'b')
  C.setopt(name, opt_value_to_string(C.optinfo(name), v), scope, '=')
end
function api.nvim_get_option(name) return C.getopt(name, 'g') end
function api.nvim_set_option(name, v) C.setopt(name, v, 'g', '=') end
function api.nvim_buf_get_option(b, name) return C.getopt(name, 'l') end
function api.nvim_buf_set_option(b, name, v) C.setopt(name, v, 'l', '=') end
function api.nvim_win_get_option(w, name) return C.getopt(name, 'l') end
function api.nvim_win_set_option(w, name, v) C.setopt(name, v, 'l', '=') end
function api.nvim_get_var(n) return vim.g[n] end
function api.nvim_set_var(n, v) vim.g[n] = v end
function api.nvim_del_var(n) vim.g[n] = nil end
function api.nvim_buf_get_var(b, n) return vim._bufvars(b == 0 and C.curbuf() or b)[n] end
function api.nvim_buf_set_var(b, n, v) vim._bufvars(b == 0 and C.curbuf() or b)[n] = v end
function api.nvim_get_mode() return { mode = vim.fn.mode(), blocking = false } end
function api.nvim_set_keymap(mode, lhs, rhs, opts)
  opts = opts or {}
  local o = { silent = opts.silent, expr = opts.expr, nowait = opts.nowait, remap = not opts.noremap }
  vim.keymap.set(mode == '' and { 'n', 'v', 'o' } or (mode == '!' and { 'i', 'c' } or mode), lhs, opts.callback or rhs, o)
end
function api.nvim_buf_set_keymap(b, mode, lhs, rhs, opts)
  opts = opts or {}
  local o = { silent = opts.silent, expr = opts.expr, nowait = opts.nowait, remap = not opts.noremap, buffer = b }
  vim.keymap.set(mode, lhs, opts.callback or rhs, o)
end
function api.nvim_del_keymap(mode, lhs) vim.keymap.del(mode, lhs) end
function api.nvim_buf_del_keymap(b, mode, lhs) vim.keymap.del(mode, lhs, { buffer = b }) end
function api.nvim_strwidth(s) return vim.fn.strdisplaywidth(s) end
function api.nvim_list_runtime_paths() return C.runtime_paths() end

-------------------------------------------------------------------------------
-- utilities (subset of Neovim's vim.* helpers)
-------------------------------------------------------------------------------
function vim.keycode(s) return C.keycode(s) end

function vim.inspect(x, opts, indent, seen)
  indent = indent or ''
  seen = seen or {}
  local t = type(x)
  if t == 'string' then return string.format('%q', x) end
  if t ~= 'table' then return tostring(x) end
  if seen[x] then return '<cycle>' end
  seen[x] = true
  local parts = {}
  local n = #x
  for i = 1, n do parts[#parts + 1] = vim.inspect(x[i], opts, indent .. '  ', seen) end
  local keys = {}
  for k in pairs(x) do
    if not (math.type(k) == 'integer' and k >= 1 and k <= n) then keys[#keys + 1] = k end
  end
  table.sort(keys, function(a, b) return tostring(a) < tostring(b) end)
  for _, k in ipairs(keys) do
    local ks = (type(k) == 'string' and k:match('^[%a_][%w_]*$')) and k or ('[' .. vim.inspect(k) .. ']')
    parts[#parts + 1] = ks .. ' = ' .. vim.inspect(x[k], opts, indent .. '  ', seen)
  end
  seen[x] = nil
  if #parts == 0 then return '{}' end
  local one = '{ ' .. table.concat(parts, ', ') .. ' }'
  if #one <= 70 and not one:find('\n') then return one end
  return '{\n' .. indent .. '  ' .. table.concat(parts, ',\n' .. indent .. '  ') .. '\n' .. indent .. '}'
end

function vim.print(...)
  local parts = {}
  for i = 1, select('#', ...) do
    local v = select(i, ...)
    parts[#parts + 1] = type(v) == 'string' and v or vim.inspect(v)
  end
  C.echo(table.concat(parts, '\n'), nil, true)
  return ...
end
_G.print = function(...)
  local parts = {}
  for i = 1, select('#', ...) do parts[#parts + 1] = tostring((select(i, ...))) end
  C.echo(table.concat(parts, '\t'), nil, true)
end

vim.log = { levels = { TRACE = 0, DEBUG = 1, INFO = 2, WARN = 3, ERROR = 4, OFF = 5 } }
function vim.notify(msg, level)
  local hl = nil
  if level == vim.log.levels.ERROR then hl = 'ErrorMsg' elseif level == vim.log.levels.WARN then hl = 'WarningMsg' end
  C.echo(tostring(msg), hl, true)
end
vim.notify_once = vim.notify

function vim.split(s, sep, opts)
  opts = opts or {}
  local plain = opts.plain
  local out = {}
  local start = 1
  if sep == '' then
    for c in s:gmatch('.') do out[#out + 1] = c end
    return out
  end
  while true do
    local a, b = s:find(sep, start, plain)
    if not a then out[#out + 1] = s:sub(start); break end
    out[#out + 1] = s:sub(start, a - 1)
    start = b + 1
  end
  if opts.trimempty then
    while out[1] == '' do table.remove(out, 1) end
    while out[#out] == '' do out[#out] = nil end
  end
  return out
end
function vim.gsplit(s, sep, opts)
  local parts = vim.split(s, sep, opts)
  local i = 0
  return function() i = i + 1; return parts[i] end
end
function vim.trim(s) return (s:gsub('^%s+', ''):gsub('%s+$', '')) end
function vim.startswith(s, p) return s:sub(1, #p) == p end
function vim.endswith(s, p) return p == '' or s:sub(-#p) == p end
function vim.pesc(s) return (s:gsub('[%(%)%.%%%+%-%*%?%[%]%^%$]', '%%%1')) end
function vim.tbl_keys(t) local r = {}; for k in pairs(t) do r[#r + 1] = k end return r end
function vim.tbl_values(t) local r = {}; for _, v in pairs(t) do r[#r + 1] = v end return r end
function vim.tbl_isempty(t) return next(t) == nil end
function vim.tbl_count(t) local n = 0; for _ in pairs(t) do n = n + 1 end return n end
function vim.tbl_contains(t, v) for _, x in pairs(t) do if x == v then return true end end return false end
vim.list_contains = vim.tbl_contains
function vim.tbl_map(f, t) local r = {}; for k, v in pairs(t) do r[k] = f(v) end return r end
function vim.tbl_filter(f, t) local r = {}; for _, v in ipairs(t) do if f(v) then r[#r + 1] = v end end return r end
function vim.list_extend(dst, src, s, e)
  for i = s or 1, e or #src do dst[#dst + 1] = src[i] end
  return dst
end
function vim.deepcopy(x) return copy(x, true) end
local function tbl_extend(deep, behavior, ...)
  local r = {}
  for i = 1, select('#', ...) do
    local t = select(i, ...)
    if t then
      for k, v in pairs(t) do
        if deep and type(v) == 'table' and type(r[k]) == 'table' and not is_list(v) then
          r[k] = tbl_extend(true, behavior, r[k], v)
        elseif r[k] == nil or behavior == 'force' then
          r[k] = (deep and type(v) == 'table') and copy(v, true) or v
        elseif behavior == 'error' then
          error('key found in more than one map: ' .. tostring(k))
        end
      end
    end
  end
  return r
end
function vim.tbl_extend(behavior, ...) return tbl_extend(false, behavior, ...) end
function vim.tbl_deep_extend(behavior, ...) return tbl_extend(true, behavior, ...) end
function vim.tbl_islist(t) return type(t) == 'table' and is_list(t) end
vim.islist = vim.tbl_islist
function vim.is_callable(f) return type(f) == 'function' or (type(f) == 'table' and getmetatable(f) and getmetatable(f).__call ~= nil) end
function vim.validate() end
function vim.has(f) return vim.fn.has(f) end
function vim.version() return { major = 0, minor = 1, patch = 0, fevim = true } end

-- deferred execution
vim._scheduled = {}
vim._timers = {}
local next_timer = 1
function vim.schedule(f) vim._scheduled[#vim._scheduled + 1] = f end
function vim.schedule_wrap(f) return function(...) local a = { ... }; vim.schedule(function() f(unpack(a)) end) end end
function vim.defer_fn(f, ms)
  local id = next_timer
  next_timer = next_timer + 1
  vim._timers[id] = { due = C.now_ms() + (ms or 0), f = f }
  return id
end
function vim._tick()
  local list = vim._scheduled
  vim._scheduled = {}
  for _, f in ipairs(list) do
    local ok, e = pcall(f)
    if not ok then C.echo(V.errmsg(e), 'ErrorMsg', true) end
  end
  local now = C.now_ms()
  local pending = false
  for id, t in pairs(vim._timers) do
    if t.due <= now then
      vim._timers[id] = nil
      local ok, e = pcall(t.f)
      if not ok then C.echo(V.errmsg(e), 'ErrorMsg', true) end
    else
      pending = true
    end
  end
  return pending or #vim._scheduled > 0
end

-- minimal JSON
vim.json = {}
function vim.json.encode(x)
  local t = type(x)
  if t == 'nil' or x == vim.NIL then return 'null' end
  if t == 'boolean' then return tostring(x) end
  if t == 'number' then return V.str(x) end
  if t == 'string' then
    return '"' .. x:gsub('[%c"\\]', function(c)
      local m = { ['"'] = '\\"', ['\\'] = '\\\\', ['\n'] = '\\n', ['\r'] = '\\r', ['\t'] = '\\t' }
      return m[c] or string.format('\\u%04x', c:byte())
    end) .. '"'
  end
  if t == 'table' then
    local parts = {}
    if is_list(x) then
      for i, v in ipairs(x) do parts[i] = vim.json.encode(v) end
      return '[' .. table.concat(parts, ',') .. ']'
    end
    for k, v in pairs(x) do parts[#parts + 1] = vim.json.encode(tostring(k)) .. ':' .. vim.json.encode(v) end
    return '{' .. table.concat(parts, ',') .. '}'
  end
  error('cannot encode ' .. t)
end
function vim.json.decode(s)
  local i = 1
  local function ws() i = s:find('[^%s]', i) or #s + 1 end
  local val
  local function str()
    local out = {}
    i = i + 1
    while true do
      local c = s:sub(i, i)
      if c == '' then error('unterminated string') end
      if c == '"' then i = i + 1; break end
      if c == '\\' then
        local n = s:sub(i + 1, i + 1)
        local m = { n = '\n', t = '\t', r = '\r', b = '\b', f = '\f', ['/'] = '/', ['\\'] = '\\', ['"'] = '"' }
        if n == 'u' then out[#out + 1] = utf8.char(tonumber(s:sub(i + 2, i + 5), 16)); i = i + 6
        else out[#out + 1] = m[n] or n; i = i + 2 end
      else out[#out + 1] = c; i = i + 1 end
    end
    return table.concat(out)
  end
  function val()
    ws()
    local c = s:sub(i, i)
    if c == '{' then
      local r = V.dict({})
      i = i + 1; ws()
      if s:sub(i, i) == '}' then i = i + 1; return r end
      while true do
        ws(); local k = str(); ws(); i = i + 1
        r[k] = val(); ws()
        local d = s:sub(i, i); i = i + 1
        if d == '}' then return r end
      end
    elseif c == '[' then
      local r = V.list({})
      i = i + 1; ws()
      if s:sub(i, i) == ']' then i = i + 1; return r end
      while true do
        r[#r + 1] = val(); ws()
        local d = s:sub(i, i); i = i + 1
        if d == ']' then return r end
      end
    elseif c == '"' then return str()
    elseif s:sub(i, i + 3) == 'true' then i = i + 4; return true
    elseif s:sub(i, i + 4) == 'false' then i = i + 5; return false
    elseif s:sub(i, i + 3) == 'null' then i = i + 4; return vim.NIL
    else
      local num = s:match('^-?%d+%.?%d*[eE]?[-+]?%d*', i)
      i = i + #num
      return math.tointeger(tonumber(num)) or tonumber(num)
    end
  end
  return val()
end

vim.fs = {}
function vim.fs.joinpath(...) return (table.concat({ ... }, '/'):gsub('//+', '/')) end
function vim.fs.normalize(p) return (vim.fn.expand(p):gsub('\\', '/')) end
function vim.fs.basename(p) return (p:gsub('[/\\]+$', ''):match('[^/\\]*$')) end
function vim.fs.dirname(p) return vim.fn.fnamemodify(p, ':h') end

-- fevim specific helpers
vim.fevim = {}
function vim.fevim.import_vimrc(src, dst) C.cmd('ImportVimrc! ' .. src .. (dst and (' ' .. dst) or '')) end
function vim.fevim.source_vim(path) C.cmd('source ' .. path) end
