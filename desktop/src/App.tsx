import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Activity,
  ArrowDownLeft,
  ArrowUpRight,
  AudioLines,
  DoorOpen,
  Hash,
  MessageSquareText,
  Radio,
  RadioTower,
  Send,
  UsersRound,
  X,
} from "lucide-react";
import { FormEvent, KeyboardEvent, useEffect, useRef, useState } from "react";

type WorkerEvent =
  | { kind: "message_sent"; bytes: number }
  | { kind: "message_received"; sender: string; address: string; args: string[] }
  | { kind: "error"; message: string }
  | { kind: "stopped" }
  | { kind: "dropped_events"; total: number };

type ChatMessage = {
  id: string;
  senderId: string;
  text: string;
  room: string | null;
  receivedAt: number;
};

type Contact = { id: string; name: string; application: string };

type SavedIdentity = {
  clientName: string;
  clientUuid: string;
};

const IDENTITY_KEY = "oscmessenger.identity.v1";
const RECENT_ROOMS_KEY = "oscmessenger.recent-rooms.v1";
const MAX_RECENT_ROOMS = 20;
const GLOBAL_PRESENCE_QUERY_ROOM = "__OSCMESSENGER_GLOBAL_PRESENCE__";
const desktopBridgeAvailable = "__TAURI_INTERNALS__" in window;
const RECOMMENDED_ROOMS = [
  { name: "LX", role: "Lighting" },
  { name: "SM", role: "Stage management" },
  { name: "SX", role: "Sound" },
  { name: "MX", role: "Mechanist / staging" },
  { name: "DECK", role: "Deck crew" },
  { name: "RIGGING", role: "Rigging" },
  { name: "PROPS", role: "Props" },
  { name: "WARDROBE", role: "Wardrobe" },
  { name: "SCENIC", role: "Scenic" },
  { name: "VIDEO", role: "Video" },
  { name: "FOH", role: "Front of house" },
  { name: "PRODUCTION", role: "Production" },
];

function roomRole(roomName: string) {
  return RECOMMENDED_ROOMS.find((room) => room.name === roomName)?.role;
}

function formatRoomName(roomName: string) {
  const role = roomRole(roomName);
  return role ? `${roomName} · ${role}` : roomName;
}

function newUuid() {
  return crypto.randomUUID().toUpperCase();
}

function loadIdentity(): SavedIdentity {
  try {
    const saved = localStorage.getItem(IDENTITY_KEY);
    if (saved) {
      const parsed = JSON.parse(saved) as Partial<SavedIdentity>;
      return {
        clientName: parsed.clientName || "Stage client",
        clientUuid: parsed.clientUuid || newUuid(),
      };
    }
  } catch {
    localStorage.removeItem(IDENTITY_KEY);
  }
  return { clientName: "Stage client", clientUuid: newUuid() };
}

function loadRecentRooms(): string[] {
  try {
    const saved = localStorage.getItem(RECENT_ROOMS_KEY);
    if (!saved) return [];
    const parsed: unknown = JSON.parse(saved);
    if (!Array.isArray(parsed)) return [];

    const rooms: string[] = [];
    const seen = new Set<string>();
    for (const value of parsed) {
      if (typeof value !== "string") continue;
      const room = value.trim();
      const key = room;
      if (!room || room.includes("/") || room.includes("\0") || seen.has(key)) continue;
      seen.add(key);
      rooms.push(room);
      if (rooms.length === MAX_RECENT_ROOMS) break;
    }
    return rooms;
  } catch {
    return [];
  }
}

function roomFromAddress(address: string) {
  const prefix = "/messenger/me/room/";
  const suffix = "/say";
  if (!address.startsWith(prefix) || !address.endsWith(suffix)) return null;
  return address.slice(prefix.length, -suffix.length);
}

function roomPresenceFromAddress(address: string) {
  const prefix = "/messenger/me/room/";
  const suffix = address.startsWith(prefix) ? address.slice(prefix.length) : "";
  const action = suffix.endsWith("/enter") ? "enter" : suffix.endsWith("/exit") ? "exit" : null;
  if (!action) return null;
  const room = suffix.slice(0, -(action.length + 1));
  return room && !room.includes("/") ? { room, action } : null;
}

function formatTime(timestamp: number) {
  return new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" }).format(timestamp);
}

export default function App() {
  const [identity, setIdentity] = useState(loadIdentity);
  const [connected, setConnected] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [clientUuid, setClientUuid] = useState(identity.clientUuid);
  const [activeRoom, setActiveRoom] = useState<string | null>(null);
  const [sendToRoom, setSendToRoom] = useState(false);
  const [sendRoom, setSendRoom] = useState("");
  const [roomDraft, setRoomDraft] = useState("");
  const [recentRooms, setRecentRooms] = useState(loadRecentRooms);
  const [messageDraft, setMessageDraft] = useState("");
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [contacts, setContacts] = useState<Contact[]>([]);
  const [notice, setNotice] = useState("");
  const [droppedEvents, setDroppedEvents] = useState(0);
  const [sending, setSending] = useState(false);
  const transcriptRef = useRef<HTMLDivElement>(null);
  const startupConnectionAttempted = useRef(false);
  const connectInProgress = useRef(false);

  useEffect(() => {
    localStorage.setItem(IDENTITY_KEY, JSON.stringify(identity));
  }, [identity]);

  useEffect(() => {
    localStorage.setItem(RECENT_ROOMS_KEY, JSON.stringify(recentRooms));
  }, [recentRooms]);

  useEffect(() => {
    if (!desktopBridgeAvailable) return;
    let unlisten: (() => void) | undefined;
    let active = true;
    void listen<WorkerEvent>("worker-event", ({ payload }) => {
      if (!active) return;
      if (payload.kind === "stopped") {
        setConnected(false);
        setConnecting(false);
        setActiveRoom(null);
        setSendToRoom(false);
        setSendRoom("");
        setContacts([]);
        return;
      }
      if (payload.kind === "error") {
        setNotice(payload.message);
        return;
      }
      if (payload.kind === "dropped_events") {
        setDroppedEvents(payload.total);
        return;
      }
      if (payload.kind !== "message_received") return;

      const { address, args } = payload;
      if (address === "/messenger/me/farewell" && args.length >= 1) {
        setContacts((current) => current.filter((contact) => contact.id !== args[0]));
        return;
      }

      const roomPresence = roomPresenceFromAddress(address);
      if (roomPresence?.room === activeRoom && args.length >= 3 && args[0] !== clientUuid) {
        const [id, name, application] = args;
        if (roomPresence.action === "exit") {
          setContacts((current) => current.filter((contact) => contact.id !== id));
        } else {
          setContacts((current) => current.some((contact) => contact.id === id)
            ? current.map((contact) => contact.id === id ? { ...contact, name, application } : contact)
            : [...current, { id, name, application }]);
        }
        return;
      }

      const room = roomFromAddress(address);
      if ((address === "/messenger/me/say" || room !== null) && args.length >= 3) {
        const [id, senderId, text] = args;
        setMessages((current) => current.some((message) => message.id === id)
          ? current
          : [...current, { id, senderId, text, room, receivedAt: Date.now() }]);
        return;
      }

      const globalListening = address === "/messenger/me/listening";
      const roomListeningPrefix = "/messenger/me/listening/room/";
      const inCurrentRoom = activeRoom && address === `${roomListeningPrefix}${activeRoom}`;
      if ((globalListening || inCurrentRoom) && args.length >= 3) {
        const [id, name, application] = args;
        if (id !== clientUuid) {
          setContacts((current) => current.some((contact) => contact.id === id)
            ? current.map((contact) => contact.id === id ? { ...contact, name, application } : contact)
            : [...current, { id, name, application }]);
        }
      }
    }).then((stop) => {
      if (active) unlisten = stop;
      else stop();
    }).catch((error: unknown) => setNotice(String(error)));
    return () => {
      active = false;
      unlisten?.();
    };
  }, [activeRoom, clientUuid]);

  useEffect(() => {
    transcriptRef.current?.scrollTo({ top: transcriptRef.current.scrollHeight, behavior: "smooth" });
  }, [messages, activeRoom]);

  const displayRoom = activeRoom ?? (sendToRoom ? sendRoom.trim() || null : null);
  const visibleMessages = messages.filter((message) => message.room === displayRoom);
  const visibleContacts = contacts;
  const roomSearch = roomDraft.trim().toLowerCase();
  const recentRoomSuggestions = recentRooms
    .filter((room) => room.toLowerCase().includes(roomSearch))
    .map((name) => ({
      name,
      role: roomRole(name) ?? "Recent room",
    }));
  const recentRoomNames = new Set(recentRooms);
  const recommendedRoomSuggestions = RECOMMENDED_ROOMS.filter((room) =>
    !recentRoomNames.has(room.name) && room.name.toLowerCase().includes(roomSearch));

  function updateIdentity(field: "clientName", value: string) {
    setIdentity((current) => ({ ...current, [field]: value }));
  }

  async function connect() {
    if (!desktopBridgeAvailable || connected || connectInProgress.current) return;
    if (!identity.clientName.trim()) {
      setNotice("Add a callsign first.");
      return;
    }
    connectInProgress.current = true;
    setConnecting(true);
    setNotice("");
    try {
      const result = await invoke<{ client_uuid: string }>("connect", {
        clientName: identity.clientName.trim(),
        clientUuid: identity.clientUuid,
      });
      setClientUuid(result.client_uuid);
      setIdentity((current) => ({ ...current, clientUuid: result.client_uuid }));
      setConnected(true);
      await invoke("query_listening", { roomName: GLOBAL_PRESENCE_QUERY_ROOM, withinRoom: false });
      setNotice("");
    } catch (error) {
      setNotice(String(error));
    } finally {
      connectInProgress.current = false;
      setConnecting(false);
    }
  }

  useEffect(() => {
    if (!desktopBridgeAvailable || startupConnectionAttempted.current) return;
    startupConnectionAttempted.current = true;
    void connect();
  }, []);

  async function disconnect() {
    try {
      await invoke("disconnect");
      setConnected(false);
      setActiveRoom(null);
      setSendToRoom(false);
      setSendRoom("");
      setContacts([]);
    } catch (error) {
      setNotice(String(error));
    }
  }

  async function joinRoom(roomName: string) {
    const room = roomName.trim();
    if (!room || room.includes("/") || room.includes("\0")) {
      setNotice("Room names cannot be empty or contain a slash.");
      return;
    }
    if (!connected) {
      setNotice("Connect before entering a room.");
      return;
    }
    if (activeRoom === room) {
      setRoomDraft("");
      return;
    }
    try {
      if (activeRoom && activeRoom !== room) {
        await invoke("exit_room", { roomName: activeRoom });
      }
      await invoke("enter_room", { roomName: room });
      setActiveRoom(room);
      setSendToRoom(false);
      setSendRoom("");
      setRoomDraft("");
      setRecentRooms((current) => [room, ...current.filter((recent) => recent !== room)].slice(0, MAX_RECENT_ROOMS));
      setContacts([]);
      await invoke("query_listening", { roomName: room, withinRoom: true });
      setNotice("");
    } catch (error) {
      setNotice(String(error));
    }
  }

  async function enterRoom(event: FormEvent) {
    event.preventDefault();
    await joinRoom(roomDraft);
  }

  async function leaveRoom() {
    if (!activeRoom) return;
    try {
      await invoke("exit_room", { roomName: activeRoom });
      setActiveRoom(null);
      setContacts([]);
      await invoke("query_listening", { roomName: GLOBAL_PRESENCE_QUERY_ROOM, withinRoom: false });
      setNotice("");
    } catch (error) {
      setNotice(String(error));
    }
  }

  async function selectGlobalChannel() {
    setSendToRoom(false);
    setSendRoom("");
    if (activeRoom) await leaveRoom();
  }

  async function sendMessage(event: FormEvent) {
    event.preventDefault();
    const text = messageDraft.trim();
    if (!text || !connected || sending) return;
    const targetRoom = activeRoom ?? (sendToRoom ? sendRoom.trim() : null);
    if (sendToRoom && (!targetRoom || targetRoom.includes("/"))) {
      setNotice("Enter a room name without a slash.");
      return;
    }
    const id = newUuid();
    setSending(true);
    try {
      await invoke("send_message", { messageUuid: id, text, roomName: targetRoom });
      setMessages((current) => current.some((message) => message.id === id)
        ? current
        : [...current, { id, senderId: clientUuid, text, room: targetRoom, receivedAt: Date.now() }]);
      setMessageDraft("");
      setNotice("");
    } catch (error) {
      setNotice(String(error));
    } finally {
      setSending(false);
    }
  }

  function submitOnEnter(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      event.currentTarget.form?.requestSubmit();
    }
  }

  function renderRoomSuggestion(room: { name: string; role: string }) {
    return <button
      type="button"
      key={room.name.toLowerCase()}
      className={`recommended-room ${activeRoom === room.name ? "selected" : ""}`}
      onClick={() => void joinRoom(room.name)}
      disabled={!connected}
      title={room.role}
      aria-label={`Enter ${room.name}, ${room.role}`}
    ><span className="recommended-room-name">#{room.name}</span><span className="recommended-room-role">{room.role}</span></button>;
  }

  return (
    <main className={`app-shell ${connected ? "is-connected" : "is-disconnected"}`}>
      <aside className="sidebar">
        <div className="brand-lockup">
          <div className="brand-mark"><AudioLines size={21} strokeWidth={2.2} /></div>
          <div><span className="brand-name">OSC<span>Messenger</span></span><span className="brand-edition">STAGE COMMS / 01</span></div>
        </div>

        <div className="connection-block">
          <div className="section-label">YOUR STATION</div>
          <label className="field-label" htmlFor="client-name">Callsign</label>
          <input id="client-name" value={identity.clientName} maxLength={48} onChange={(event) => updateIdentity("clientName", event.target.value)} disabled={connected} />
          <button className={`connection-button ${connected ? "is-connected" : ""}`} onClick={connected ? disconnect : connect} disabled={!desktopBridgeAvailable || connecting}>
            <span className="connection-indicator" />
            {connecting ? "Connecting…" : connected ? "Disconnect" : "Connect to OSC"}
            {connected ? <X size={15} /> : <RadioTower size={15} />}
          </button>
          <div className="connection-meta"><span>{connected ? "MULTICAST LINK ACTIVE" : connecting ? "CONNECTING TO OSC" : desktopBridgeAvailable ? "LINK STANDBY" : "NATIVE CLIENT REQUIRED"}</span><span className="mono">239.254.0.1 : 3090</span></div>
        </div>

        {connected && <div className="channels-block">
          <div className="section-heading"><span className="section-label">CHANNEL</span><span className="live-dot" /></div>
          <button type="button" className={`channel-row channel-row-button ${!activeRoom && !sendToRoom ? "selected" : ""}`} onClick={() => void selectGlobalChannel()} aria-current={!activeRoom && !sendToRoom ? "page" : undefined} title="Switch to global channel">
            <MessageSquareText size={16} /><span>Global</span><span className="channel-tag">ALL</span>
          </button>
          {activeRoom && <div className="channel-row selected room-active"><Hash size={16} /><span className="active-room-copy"><strong>#{activeRoom}</strong><small>{roomRole(activeRoom) ?? "Custom room"}</small></span><button aria-label="Leave room" title="Leave room" onClick={leaveRoom}><DoorOpen size={15} /></button></div>}
          <form className="room-form" onSubmit={enterRoom}>
            <label htmlFor="room-name">ENTER A ROOM</label>
            <div className="room-input-wrap"><Hash size={15} /><input id="room-name" value={roomDraft} onChange={(event) => setRoomDraft(event.target.value)} placeholder="room name" maxLength={64} /><button title="Enter room" aria-label="Enter room" disabled={!connected || !roomDraft.trim()}><ArrowUpRight size={16} /></button></div>
          </form>
          <div className="recommended-rooms">
            {recentRoomSuggestions.length > 0 && <div className="room-suggestion-section">
              <div className="recommended-heading">RECENT ROOMS</div>
              <div className="recommended-grid">{recentRoomSuggestions.map(renderRoomSuggestion)}</div>
            </div>}
            {recommendedRoomSuggestions.length > 0 && <div className="room-suggestion-section">
              <div className="recommended-heading">{roomSearch ? "MATCHING ROOMS" : "RECOMMENDED ROOMS"}</div>
              <div className="recommended-grid">{recommendedRoomSuggestions.map(renderRoomSuggestion)}</div>
            </div>}
            {roomSearch && recentRoomSuggestions.length === 0 && recommendedRoomSuggestions.length === 0 && <div className="no-room-matches">No matching rooms</div>}
          </div>
        </div>}

        <div className="sidebar-bottom">
          <div className="network-status"><span className={connected ? "status-led active" : "status-led"} /><span>{connected ? "OSC NETWORK" : "OFFLINE"}</span><span className="mono">UDP</span></div>
          <div className="client-id"><span>CLIENT UUID</span><code>{clientUuid.slice(0, 8)}…{clientUuid.slice(-4)}</code></div>
        </div>
      </aside>

      <section className="main-panel">
        <header className="topbar">
          <div className="channel-title"><div className="channel-icon">{displayRoom ? <Hash size={18} /> : <Radio size={18} />}</div><div><h1>{displayRoom ? formatRoomName(displayRoom) : "Global channel"}</h1><p>{activeRoom ? "ROOM TRANSMISSION" : sendToRoom ? "ROOM SEND ONLY" : "OPEN TRANSMISSION"}<span className="title-separator">/</span>{connected ? "LISTENING" : "AWAITING CONNECTION"}</p></div></div>
          <div className="topbar-right"><span className="signal-label"><Activity size={14} /> {connected ? "SIGNAL READY" : connecting ? "CONNECTING" : "NO SIGNAL"}</span><span className="topbar-divider" /><span className="protocol-label">OSC <b>1.0</b></span></div>
        </header>

        {!desktopBridgeAvailable && <div className="browser-notice">Open OSCMessenger as a desktop app to connect to the multicast network.</div>}
        {notice && <div className="notice" role="status"><span>{notice}</span><button title="Dismiss" aria-label="Dismiss" onClick={() => setNotice("")}><X size={15} /></button></div>}
        {connected && droppedEvents > 0 && <div className="drop-warning"><ArrowDownLeft size={15} /> {droppedEvents} network event{droppedEvents === 1 ? "" : "s"} dropped while the queue was full.</div>}

        {connected ? <div className="transcript" ref={transcriptRef}>
          {visibleMessages.length === 0 ? (
            <div className="empty-state">
              <div className="empty-mark"><AudioLines size={27} /></div>
              <div className="empty-kicker">{connected ? "CHANNEL OPEN" : "STANDING BY"}</div>
              <h2>{connected ? "Quiet on comms." : "Ready when you are."}</h2>
              <p>{connected ? "Messages on this channel will appear here." : "Connect to the OSC network to start sending and receiving."}</p>
              <div className="empty-rule"><span /><span /><span /></div>
            </div>
          ) : (
            <div className="message-list">
              <div className="day-marker"><span /> LIVE TRANSMISSION <span /></div>
              {visibleMessages.map((message, index) => {
                const own = message.senderId === clientUuid;
                const contact = contacts.find((item) => item.id === message.senderId);
                return <article className={`message ${own ? "own-message" : ""}`} key={message.id} style={{ animationDelay: `${Math.min(index, 8) * 35}ms` }}>
                  <div className={`avatar ${own ? "own-avatar" : ""}`}>{(own ? identity.clientName : contact?.name || message.senderId).slice(0, 1).toUpperCase()}</div>
                  <div className="message-content"><div className="message-meta"><strong>{own ? `${identity.clientName} (you)` : contact?.name || message.senderId.slice(0, 8)}</strong><span>{formatTime(message.receivedAt)}</span>{!own && contact && <span className="app-tag">{contact.application}</span>}</div><p>{message.text}</p></div>
                  <span className="message-kind">{own ? "OUT" : "IN"}</span>
                </article>;
              })}
            </div>
          )}
        </div> : <div className="connection-state">
          <div className="empty-mark"><RadioTower size={25} /></div>
          <div className="empty-kicker">{!desktopBridgeAvailable ? "BROWSER PREVIEW" : connecting ? "CONNECTING TO OSC" : "OSC LINK OFFLINE"}</div>
          <h2>{!desktopBridgeAvailable ? "Native client required." : connecting ? "Looking for the OSC network." : "Waiting for connection."}</h2>
          <p>{!desktopBridgeAvailable ? "Open the desktop client to send messages and enter rooms." : connecting ? "Chat and room controls will appear when the link is ready." : "Use Connect to OSC to retry the network connection."}</p>
        </div>}

        {connected && <footer className="composer-wrap">
          <form className="composer" onSubmit={sendMessage}>
            <div className="composer-leading"><span className="composer-dot" />{activeRoom ? <span>#{activeRoom} · {roomRole(activeRoom) ?? "Room"}</span> : <><select aria-label="Message destination" value={sendToRoom ? "room" : "global"} onChange={(event) => setSendToRoom(event.target.value === "room")} disabled={!connected}><option value="global">GLOBAL</option><option value="room">ROOM</option></select>{sendToRoom && <input className="target-room-input" aria-label="Room message destination" placeholder="room name" value={sendRoom} onChange={(event) => setSendRoom(event.target.value)} maxLength={64} disabled={!connected} />}</>}</div>
            <textarea value={messageDraft} onChange={(event) => setMessageDraft(event.target.value)} onKeyDown={submitOnEnter} placeholder={connected ? "Transmit a message…" : "Connect to transmit"} maxLength={2048} rows={1} disabled={!connected} aria-label="Message" />
            <button type="submit" className="send-button" aria-label="Send message" title="Send message" disabled={!connected || !messageDraft.trim() || sending}><Send size={17} /></button>
          </form>
          <div className="composer-hint"><span>RETURN TO SEND <kbd>↵</kbd></span><span>SHIFT + RETURN FOR NEW LINE</span>{sendToRoom && !activeRoom && <span className="direct-send-note">NOT JOINED</span>}<span className="char-count">{messageDraft.length}/2048</span></div>
        </footer>}
      </section>

      {connected && <aside className="details-panel">
          <div className="details-header"><div className="section-label">CHANNEL DETAILS</div><div className="details-channel"><div className="details-channel-icon">{displayRoom ? <Hash size={16} /> : <Radio size={16} />}</div><div><strong>{displayRoom ? formatRoomName(displayRoom) : "Global"}</strong><span>{activeRoom ? "ROOM" : sendToRoom ? "DIRECT TARGET" : "MULTICAST"}</span></div></div></div>
        <div className="roster-section"><div className="roster-heading"><span>ON CHANNEL</span><span className="roster-count">{visibleContacts.length + (connected ? 1 : 0)}</span></div>
          {connected && <div className="roster-person"><div className="roster-avatar local">{identity.clientName.slice(0, 1).toUpperCase()}</div><div className="roster-copy"><strong>{identity.clientName || "Unnamed station"}</strong><span>YOU</span></div><span className="roster-presence" /></div>}
          {visibleContacts.map((contact) => <div className="roster-person" key={contact.id}><div className="roster-avatar">{contact.name.slice(0, 1).toUpperCase()}</div><div className="roster-copy"><strong>{contact.name}</strong><span>{contact.application}</span></div><span className="roster-presence" /></div>)}
          {!connected && <div className="roster-empty"><UsersRound size={17} /><span>No stations connected</span></div>}
          {connected && visibleContacts.length === 0 && <div className="roster-empty"><UsersRound size={17} /><span>Only you, for now</span></div>}
        </div>
        <div className="transport-section"><div className="section-label">TRANSPORT</div><div className="transport-row"><span>Protocol</span><strong>OSC over UDP</strong></div><div className="transport-row"><span>Multicast group</span><strong className="mono">239.254.0.1</strong></div><div className="transport-row"><span>Port</span><strong className="mono">3090</strong></div><div className="transport-row"><span>Client ID</span><strong className="mono">{clientUuid.slice(0, 8)}</strong></div></div>
        <div className="details-footer"><DoorOpen size={14} /><span>{sendToRoom && !activeRoom ? "DIRECT ROOM SEND DOES NOT JOIN THE ROOM" : "ROOM MEMBERSHIP IS LOCAL TO THIS CLIENT"}</span></div>
      </aside>}
    </main>
  );
}