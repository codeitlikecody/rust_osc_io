import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import {
  Activity,
  ArrowDownLeft,
  ArrowUpRight,
  AudioLines,
  DoorOpen,
  Hash,
  ArrowLeft,
  Cable,
  Menu,
  MessageSquareText,
  Radio,
  RadioTower,
  Send,
  Settings2,
  Sun,
  Moon,
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
type LocalNetworkInterface = { name: string; address: string; is_loopback: boolean };
type GatewayUiStatus = { state: string; gateway_uuid: string | null; detail: string | null };
type RoomState = { active_room: string | null; revision: number };

type SavedIdentity = {
  clientName: string;
  clientUuid: string;
};

type Preferences = {
  networkMode: "multicast" | "unicast";
  interfaceMode: "default" | "all" | "selected";
  multicastInterfaces: string[];
  multicastGroup: string;
  listenerPort: string;
  unicastAddress: string;
  unicastPort: string;
  theme: "dark" | "light";
  accent: "lime" | "cyan" | "amber" | "rose";
  density: "comfortable" | "compact";
  reduceMotion: boolean;
  autoConnect: boolean;
  serveWebInterface: boolean;
  gatewayEnabled: boolean;
  gatewayConflictPolicy: "start_anyway" | "stop_local" | "never_start";
  enterToSend: boolean;
  showTimestamps: boolean;
  showPeerApplications: boolean;
  showRecentRooms: boolean;
  recentRoomLimit: number;
  showRecommendedRooms: boolean;
  recommendedRoomLimit: number;
};

const IDENTITY_KEY = "oscmessenger.identity.v1";
const RECENT_ROOMS_KEY = "oscmessenger.recent-rooms.v1";
const PREFERENCES_KEY = "oscmessenger.preferences.v1";
const MAX_RECENT_ROOMS = 20;
const GLOBAL_PRESENCE_QUERY_ROOM = "__OSCMESSENGER_GLOBAL_PRESENCE__";
const nativeBridgeAvailable = "__TAURI_INTERNALS__" in window;
const browserBridgeAvailable = "__OSCMESSENGER_WEB_BRIDGE__" in window;
const desktopBridgeAvailable = nativeBridgeAvailable || browserBridgeAvailable;
const DEFAULT_PREFERENCES: Preferences = {
  networkMode: "multicast",
  interfaceMode: "default",
  multicastInterfaces: [],
  multicastGroup: "239.254.0.1",
  listenerPort: "3090",
  unicastAddress: "127.0.0.1",
  unicastPort: "3090",
  theme: "dark",
  accent: "lime",
  density: "comfortable",
  reduceMotion: false,
  autoConnect: true,
  serveWebInterface: true,
  gatewayEnabled: true,
  gatewayConflictPolicy: "stop_local",
  enterToSend: true,
  showTimestamps: true,
  showPeerApplications: true,
  showRecentRooms: true,
  recentRoomLimit: MAX_RECENT_ROOMS,
  showRecommendedRooms: true,
  recommendedRoomLimit: 12,
};
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

function invoke<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (nativeBridgeAvailable) return tauriInvoke<T>(command, args);
  if (!browserBridgeAvailable) return Promise.reject(new Error("Desktop bridge is unavailable."));
  return fetch(`/api/commands/${encodeURIComponent(command)}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(args),
  }).then(async (response) => {
    const text = await response.text();
    let payload: unknown;
    try {
      payload = text ? JSON.parse(text) : null;
    } catch {
      payload = text;
    }
    if (!response.ok) {
      const error = payload && typeof payload === "object" && "error" in payload
        ? String((payload as { error: unknown }).error)
        : String(payload || response.statusText);
      throw new Error(error);
    }
    return payload as T;
  });
}

function listen<T>(eventName: string, handler: (event: { payload: T }) => void): Promise<() => void> {
  if (nativeBridgeAvailable) return tauriListen<T>(eventName, handler);
  if (!browserBridgeAvailable) return Promise.reject(new Error("Desktop bridge is unavailable."));
  return new Promise((resolve, reject) => {
    const eventUrl = new URL("/api/events", window.location.href);
    eventUrl.protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    const socket = new WebSocket(eventUrl);
    socket.onopen = () => resolve(() => socket.close());
    socket.onerror = () => reject(new Error("Could not connect to the OSCMessenger event stream."));
    socket.onmessage = ({ data }) => {
      try {
        const event = JSON.parse(String(data)) as { event: string; payload: T };
        if (event.event === eventName) handler({ payload: event.payload });
      } catch {
        // Ignore malformed event frames and keep the stream alive.
      }
    };
  });
}

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

function loadPreferences(): Preferences {
  try {
    const saved = localStorage.getItem(PREFERENCES_KEY);
    if (!saved) return DEFAULT_PREFERENCES;
    const parsed = JSON.parse(saved) as Partial<Preferences>;
    return {
      networkMode: parsed.networkMode === "unicast" ? "unicast" : "multicast",
      interfaceMode: parsed.interfaceMode === "all" || parsed.interfaceMode === "selected" ? parsed.interfaceMode : "default",
      multicastInterfaces: Array.isArray(parsed.multicastInterfaces)
        ? parsed.multicastInterfaces.filter((address): address is string => typeof address === "string")
        : [],
      multicastGroup: typeof parsed.multicastGroup === "string" ? parsed.multicastGroup : DEFAULT_PREFERENCES.multicastGroup,
      listenerPort: typeof parsed.listenerPort === "string" ? parsed.listenerPort : DEFAULT_PREFERENCES.listenerPort,
      unicastAddress: typeof parsed.unicastAddress === "string" ? parsed.unicastAddress : DEFAULT_PREFERENCES.unicastAddress,
      unicastPort: typeof parsed.unicastPort === "string" ? parsed.unicastPort : DEFAULT_PREFERENCES.unicastPort,
      theme: parsed.theme === "light" ? "light" : "dark",
      accent: parsed.accent === "cyan" || parsed.accent === "amber" || parsed.accent === "rose" ? parsed.accent : "lime",
      density: parsed.density === "compact" ? "compact" : "comfortable",
      reduceMotion: parsed.reduceMotion === true,
      autoConnect: parsed.autoConnect !== false,
      serveWebInterface: parsed.serveWebInterface !== false,
      gatewayEnabled: parsed.gatewayEnabled !== false,
      gatewayConflictPolicy: parsed.gatewayConflictPolicy === "start_anyway" || parsed.gatewayConflictPolicy === "never_start"
        ? parsed.gatewayConflictPolicy
        : "stop_local",
      enterToSend: parsed.enterToSend !== false,
      showTimestamps: parsed.showTimestamps !== false,
      showPeerApplications: parsed.showPeerApplications !== false,
      showRecentRooms: parsed.showRecentRooms !== false,
      recentRoomLimit: typeof parsed.recentRoomLimit === "number" && Number.isInteger(parsed.recentRoomLimit)
        ? Math.max(1, Math.min(MAX_RECENT_ROOMS, parsed.recentRoomLimit))
        : DEFAULT_PREFERENCES.recentRoomLimit,
      showRecommendedRooms: parsed.showRecommendedRooms !== false,
      recommendedRoomLimit: typeof parsed.recommendedRoomLimit === "number" && Number.isInteger(parsed.recommendedRoomLimit)
        ? Math.max(1, Math.min(RECOMMENDED_ROOMS.length, parsed.recommendedRoomLimit))
        : DEFAULT_PREFERENCES.recommendedRoomLimit,
    };
  } catch {
    return DEFAULT_PREFERENCES;
  }
}

function isValidIpv4(value: string) {
  const octets = value.trim().split(".");
  return octets.length === 4 && octets.every((octet) => {
    if (!/^\d{1,3}$/.test(octet)) return false;
    const number = Number(octet);
    return number >= 0 && number <= 255;
  });
}

function isValidMulticastIpv4(value: string) {
  if (!isValidIpv4(value)) return false;
  const firstOctet = Number(value.trim().split(".")[0]);
  return firstOctet >= 224 && firstOctet <= 239;
}

function isValidUnicastIpv4(value: string) {
  if (!isValidIpv4(value)) return false;
  const firstOctet = Number(value.trim().split(".")[0]);
  return firstOctet > 0 && firstOctet < 224;
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
  const [preferences, setPreferences] = useState(loadPreferences);
  const [networkInterfaces, setNetworkInterfaces] = useState<LocalNetworkInterface[]>([]);
  const [networkInterfaceError, setNetworkInterfaceError] = useState("");
  const [gatewayStatus, setGatewayStatus] = useState<GatewayUiStatus>({ state: "checking", gateway_uuid: null, detail: null });
  const [webInterfaceUrl, setWebInterfaceUrl] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [mobileNavigationOpen, setMobileNavigationOpen] = useState(false);
  const [settingsError, setSettingsError] = useState("");
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
  const mobileNavigationButtonRef = useRef<HTMLButtonElement>(null);
  const mobileNavigationCloseRef = useRef<HTMLButtonElement>(null);
  const mobileNavigationRef = useRef<HTMLElement>(null);
  const startupConnectionAttempted = useRef(false);
  const connectInProgress = useRef(false);
  const gatewayStartupConfigured = useRef(false);
  const webInterfaceChange = useRef<Promise<void>>(Promise.resolve());
  const roomRevision = useRef(-1);

  useEffect(() => {
    if (!mobileNavigationOpen) return;
    mobileNavigationCloseRef.current?.focus();
    const handleKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") setMobileNavigationOpen(false);
      if (event.key !== "Tab") return;
      const focusable = Array.from(mobileNavigationRef.current?.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex='-1'])") ?? [])
        .filter((element) => element.getClientRects().length > 0);
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && (document.activeElement === first || !mobileNavigationRef.current?.contains(document.activeElement))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (document.activeElement === last || !mobileNavigationRef.current?.contains(document.activeElement))) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      mobileNavigationButtonRef.current?.focus();
    };
  }, [mobileNavigationOpen]);

  useEffect(() => {
    localStorage.setItem(IDENTITY_KEY, JSON.stringify(identity));
  }, [identity]);

  useEffect(() => {
    localStorage.setItem(RECENT_ROOMS_KEY, JSON.stringify(recentRooms));
  }, [recentRooms]);

  useEffect(() => {
    localStorage.setItem(PREFERENCES_KEY, JSON.stringify(preferences));
  }, [preferences]);

  useEffect(() => {
    if (!desktopBridgeAvailable) return;
    let active = true;
    const request = nativeBridgeAvailable
      ? webInterfaceChange.current.then(() => invoke<string | null>("configure_web_interface", { enabled: preferences.serveWebInterface }))
      : invoke<string>("get_web_interface_url");
    if (nativeBridgeAvailable) webInterfaceChange.current = request.then(() => {}, () => {});
    void request
      .then((url) => { if (active) setWebInterfaceUrl(url ?? ""); })
      .catch((error: unknown) => {
        if (active) {
          setWebInterfaceUrl("");
          if (nativeBridgeAvailable) setSettingsError(String(error));
        }
      });
    return () => { active = false; };
  }, [preferences.serveWebInterface]);

  useEffect(() => {
    if (!settingsOpen || !desktopBridgeAvailable || preferences.networkMode !== "multicast") return;
    let active = true;
    void invoke<LocalNetworkInterface[]>("get_network_interfaces")
      .then((interfaces) => {
        if (active) {
          setNetworkInterfaces(interfaces);
          setNetworkInterfaceError("");
        }
      })
      .catch((error: unknown) => {
        if (active) setNetworkInterfaceError(String(error));
      });
    return () => {
      active = false;
    };
  }, [settingsOpen, preferences.networkMode]);

  useEffect(() => {
    if (!desktopBridgeAvailable) return;
    let active = true;
    let unlisten: (() => void) | undefined;
    const applyRoomState = (roomState: RoomState) => {
      if (!active || roomState.revision <= roomRevision.current) return;
      roomRevision.current = roomState.revision;
      setActiveRoom(roomState.active_room);
      setSendToRoom(false);
      setSendRoom("");
      setContacts([]);
    };
    void listen<RoomState>("room-state", ({ payload }) => applyRoomState(payload))
      .then(async (stop) => {
        if (!active) { stop(); return; }
        unlisten = stop;
        applyRoomState(await invoke<RoomState>("get_room_state"));
      })
      .catch((error: unknown) => { if (active) setNotice(String(error)); });
    return () => { active = false; unlisten?.(); };
  }, []);

  useEffect(() => {
    if (!desktopBridgeAvailable) return;
    let active = true;
    let unlisten: (() => void) | undefined;
    void listen<GatewayUiStatus>("gateway-status", ({ payload }) => {
      if (active) setGatewayStatus(payload);
    }).then((stop) => {
      if (active) unlisten = stop;
      else stop();
    }).catch((error: unknown) => {
      if (active) setGatewayStatus({ state: "error", gateway_uuid: null, detail: String(error) });
    });
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

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
      if (active) {
        unlisten = stop;
        if (connected) {
          void invoke("query_listening", {
            roomName: activeRoom ?? GLOBAL_PRESENCE_QUERY_ROOM,
            withinRoom: activeRoom !== null,
          }).catch((error: unknown) => { if (active) setNotice(String(error)); });
        }
      }
      else stop();
    }).catch((error: unknown) => setNotice(String(error)));
    return () => {
      active = false;
      unlisten?.();
    };
  }, [activeRoom, clientUuid, connected]);

  useEffect(() => {
    transcriptRef.current?.scrollTo({
      top: transcriptRef.current.scrollHeight,
      behavior: preferences.reduceMotion ? "auto" : "smooth",
    });
  }, [messages, activeRoom, preferences.reduceMotion]);

  const displayRoom = activeRoom ?? (sendToRoom ? sendRoom.trim() || null : null);
  const visibleMessages = messages.filter((message) => message.room === displayRoom);
  const visibleContacts = contacts;
  const roomSearch = roomDraft.trim().toLowerCase();
  const recentRoomSuggestions = (preferences.showRecentRooms ? recentRooms : [])
    .filter((room) => room.toLowerCase().includes(roomSearch))
    .slice(0, preferences.recentRoomLimit)
    .map((name) => ({
      name,
      role: roomRole(name) ?? "Recent room",
    }));
  const recentRoomNames = new Set(recentRoomSuggestions.map((room) => room.name));
  const recommendedRoomSuggestions = (preferences.showRecommendedRooms ? RECOMMENDED_ROOMS : [])
    .filter((room) => !recentRoomNames.has(room.name) && room.name.toLowerCase().includes(roomSearch))
    .slice(0, preferences.recommendedRoomLimit);

  function updateIdentity(field: "clientName", value: string) {
    setIdentity((current) => ({ ...current, [field]: value }));
  }

  function updatePreference<K extends keyof Preferences>(key: K, value: Preferences[K]) {
    setPreferences((current) => ({ ...current, [key]: value }));
  }

  function resetNetworkSettings() {
    setPreferences((current) => ({
      ...current,
      networkMode: DEFAULT_PREFERENCES.networkMode,
      interfaceMode: DEFAULT_PREFERENCES.interfaceMode,
      multicastInterfaces: DEFAULT_PREFERENCES.multicastInterfaces,
      multicastGroup: DEFAULT_PREFERENCES.multicastGroup,
      listenerPort: DEFAULT_PREFERENCES.listenerPort,
      unicastAddress: DEFAULT_PREFERENCES.unicastAddress,
      unicastPort: DEFAULT_PREFERENCES.unicastPort,
    }));
    setSettingsError("");
  }

  async function multicastInterfaceAddressesFor(settings: Preferences, forGateway = false) {
    if (!forGateway && settings.networkMode !== "multicast") return [];
    if (settings.interfaceMode === "default") return [];
    if (settings.interfaceMode === "selected") return settings.multicastInterfaces;
    const interfaces = networkInterfaces.length > 0
      ? networkInterfaces
      : await invoke<LocalNetworkInterface[]>("get_network_interfaces");
    if (interfaces.length === 0) throw new Error("No IPv4 network interfaces are available.");
    return interfaces.map((networkInterface) => networkInterface.address);
  }

  async function configureGateway(settings: Preferences) {
    const multicastInterfaces = settings.gatewayEnabled
      ? await multicastInterfaceAddressesFor(settings, true)
      : [];
    if (settings.gatewayEnabled && settings.interfaceMode === "selected" && multicastInterfaces.length === 0) {
      throw new Error("Choose at least one interface for the local Gateway.");
    }
    await invoke("configure_gateway", {
      enabled: settings.gatewayEnabled,
      conflictPolicy: settings.gatewayConflictPolicy,
      multicastGroup: settings.multicastGroup.trim(),
      multicastInterfaces,
      listenerPort: Number(settings.listenerPort),
    });
    if (!settings.gatewayEnabled || settings.gatewayConflictPolicy === "never_start") {
      setGatewayStatus({ state: "disabled", gateway_uuid: null, detail: "Local Gateway disabled by settings." });
    } else {
      setGatewayStatus({ state: "checking", gateway_uuid: null, detail: null });
    }
  }

  useEffect(() => {
    if (!desktopBridgeAvailable || gatewayStartupConfigured.current) return;
    gatewayStartupConfigured.current = true;
    void configureGateway(preferences).catch((error: unknown) => {
      setGatewayStatus({ state: "error", gateway_uuid: null, detail: String(error) });
    });
  }, []);

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
      const listenerPort = Number(preferences.listenerPort);
      const unicastPort = Number(preferences.unicastPort);
      if (!Number.isInteger(listenerPort) || listenerPort < 1 || listenerPort > 65535) {
        throw new Error("Local listener port must be between 1 and 65535.");
      }
      if (preferences.networkMode === "multicast" && !isValidMulticastIpv4(preferences.multicastGroup)) {
        throw new Error("Enter an IPv4 multicast address in the 224–239 range.");
      }
      if (preferences.networkMode === "unicast" && (!isValidUnicastIpv4(preferences.unicastAddress) || !Number.isInteger(unicastPort) || unicastPort < 1 || unicastPort > 65535)) {
        throw new Error("Enter a unicast IPv4 address and destination port (1–65535).");
      }
      if (preferences.networkMode === "multicast" && preferences.interfaceMode === "selected" && preferences.multicastInterfaces.length === 0) {
        throw new Error("Choose at least one network interface.");
      }
      const multicastInterfaces = await multicastInterfaceAddressesFor(preferences);
      const result = await invoke<{ client_uuid: string }>("connect", {
        clientName: identity.clientName.trim(),
        clientUuid: identity.clientUuid,
        networkMode: preferences.networkMode,
        multicastGroup: preferences.multicastGroup.trim(),
        multicastInterfaces,
        listenerPort,
        unicastAddress: preferences.unicastAddress.trim(),
        unicastPort,
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
    if (!desktopBridgeAvailable || !preferences.autoConnect || startupConnectionAttempted.current) return;
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

  async function saveSettings(event: FormEvent) {
    event.preventDefault();
    setSettingsError("");
    const multicastGroup = preferences.multicastGroup.trim();
    const listenerPort = preferences.listenerPort.trim();
    const unicastAddress = preferences.unicastAddress.trim();
    const unicastPort = preferences.unicastPort.trim();
    const listenerPortNumber = Number(listenerPort);
    const unicastPortNumber = Number(unicastPort);
    const clientName = identity.clientName.trim();

    if (!clientName) {
      setSettingsError("Callsign cannot be empty.");
      return;
    }
    if (preferences.networkMode === "multicast" && !isValidMulticastIpv4(multicastGroup)) {
      setSettingsError("Enter an IPv4 multicast address in the 224.0.0.0–239.255.255.255 range.");
      return;
    }
    if (preferences.networkMode === "unicast" && !isValidUnicastIpv4(unicastAddress)) {
      setSettingsError("Enter a unicast IPv4 address for the target device or gateway.");
      return;
    }
    if (!/^\d+$/.test(listenerPort) || !Number.isInteger(listenerPortNumber) || listenerPortNumber < 1 || listenerPortNumber > 65535) {
      setSettingsError("Local listener port must be a whole number between 1 and 65535.");
      return;
    }
    if (preferences.networkMode === "unicast" && (!/^\d+$/.test(unicastPort) || !Number.isInteger(unicastPortNumber) || unicastPortNumber < 1 || unicastPortNumber > 65535)) {
      setSettingsError("Unicast destination port must be a whole number between 1 and 65535.");
      return;
    }
    if (preferences.networkMode === "multicast" && preferences.interfaceMode === "selected" && preferences.multicastInterfaces.length === 0) {
      setSettingsError("Choose at least one network interface.");
      return;
    }

    const nextPreferences = { ...preferences, multicastGroup, listenerPort, unicastAddress, unicastPort };
    const nextIdentity = { ...identity, clientName };
      const gatewayChanged = 
        nextPreferences.gatewayEnabled !== preferences.gatewayEnabled || 
        nextPreferences.gatewayConflictPolicy !== preferences.gatewayConflictPolicy || 
        nextPreferences.multicastGroup !== preferences.multicastGroup || 
        nextPreferences.listenerPort !== preferences.listenerPort || 
        nextPreferences.interfaceMode !== preferences.interfaceMode || 
        nextPreferences.multicastInterfaces.join(",") !== preferences.multicastInterfaces.join(","); 
    const reconnect = connected && (
      nextPreferences.networkMode !== preferences.networkMode ||
      nextPreferences.interfaceMode !== preferences.interfaceMode ||
      nextPreferences.multicastInterfaces.join(",") !== preferences.multicastInterfaces.join(",") ||
      nextPreferences.multicastGroup !== preferences.multicastGroup ||
      nextPreferences.listenerPort !== preferences.listenerPort ||
      nextPreferences.unicastAddress !== preferences.unicastAddress ||
      nextPreferences.unicastPort !== preferences.unicastPort ||
      nextIdentity.clientName !== identity.clientName
    );

    setPreferences(nextPreferences);
    setIdentity(nextIdentity);
    setSettingsOpen(false);

    if (desktopBridgeAvailable && (gatewayChanged || !gatewayStartupConfigured.current)) {
      try {
        await configureGateway(nextPreferences);
        gatewayStartupConfigured.current = true;
      } catch (error) {
        setGatewayStatus({ state: "error", gateway_uuid: null, detail: String(error) });
      }
    }

    if (!reconnect) return;
    connectInProgress.current = true;
    setConnecting(true);
    try {
      const multicastInterfaces = await multicastInterfaceAddressesFor(nextPreferences);
      await invoke("disconnect");
      setConnected(false);
      setActiveRoom(null);
      setContacts([]);
      const result = await invoke<{ client_uuid: string }>("connect", {
        clientName: nextIdentity.clientName,
        clientUuid: nextIdentity.clientUuid,
        networkMode: nextPreferences.networkMode,
        multicastGroup: nextPreferences.multicastGroup,
        multicastInterfaces,
        listenerPort: listenerPortNumber,
        unicastAddress: nextPreferences.unicastAddress,
        unicastPort: unicastPortNumber,
      });
      setClientUuid(result.client_uuid);
      setIdentity({ ...nextIdentity, clientUuid: result.client_uuid });
      setConnected(true);
      await invoke("query_listening", { roomName: GLOBAL_PRESENCE_QUERY_ROOM, withinRoom: false });
      setNotice("");
    } catch (error) {
      setConnected(false);
      setNotice(String(error));
    } finally {
      connectInProgress.current = false;
      setConnecting(false);
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
      setMobileNavigationOpen(false);
      setSettingsOpen(false);
      return;
    }
    try {
      if (activeRoom && activeRoom !== room) {
        await invoke("exit_room", { roomName: activeRoom });
      }
      await invoke("enter_room", { roomName: room });
      setRoomDraft("");
      setMobileNavigationOpen(false);
      setSettingsOpen(false);
      setRecentRooms((current) => [room, ...current.filter((recent) => recent !== room)].slice(0, MAX_RECENT_ROOMS));
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
      setMobileNavigationOpen(false);
      setSettingsOpen(false);
      setNotice("");
    } catch (error) {
      setNotice(String(error));
    }
  }

  async function selectGlobalChannel() {
    setSendToRoom(false);
    setSendRoom("");
    if (activeRoom) await leaveRoom();
    setMobileNavigationOpen(false);
    setSettingsOpen(false);
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
    if (preferences.enterToSend && event.key === "Enter" && !event.shiftKey) {
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
    <main className={`app-shell ${connected ? "is-connected" : "is-disconnected"}`} data-theme={preferences.theme} data-accent={preferences.accent} data-density={preferences.density} data-reduce-motion={preferences.reduceMotion}>
      {mobileNavigationOpen && <button type="button" className="mobile-navigation-backdrop" aria-label="Close navigation" onClick={() => setMobileNavigationOpen(false)} />}
      <aside ref={mobileNavigationRef} id="station-navigation" aria-label="Station and channels" role={mobileNavigationOpen ? "dialog" : undefined} aria-modal={mobileNavigationOpen || undefined} className={`sidebar ${mobileNavigationOpen ? "is-mobile-open" : ""}`}>
        <div className="brand-lockup">
          <div className="brand-mark"><AudioLines size={21} strokeWidth={2.2} /></div>
          <div><span className="brand-name">OSC<span>Messenger</span></span><span className="brand-edition">STAGE COMMS / 01</span></div>
          <button ref={mobileNavigationCloseRef} type="button" className="mobile-navigation-close" aria-label="Close navigation" title="Close navigation" onClick={() => setMobileNavigationOpen(false)}><X size={18} /></button>
        </div>

        <div className="connection-block">
          <div className="section-label">YOUR STATION</div>
          <div className="callsign-readout"><span>CALLSIGN</span><strong>{identity.clientName}</strong></div>
          <button className={`connection-button ${connected ? "is-connected" : ""}`} onClick={() => { setMobileNavigationOpen(false); void (connected ? disconnect() : connect()); }} disabled={!desktopBridgeAvailable || connecting}>
            <span className="connection-indicator" />
            {connecting ? "Connecting…" : connected ? "Disconnect" : "Connect to OSC"}
            {connected ? <X size={15} /> : <RadioTower size={15} />}
          </button>
          <div className="connection-meta"><span>{connected ? `${preferences.networkMode.toUpperCase()} LINK ACTIVE` : connecting ? "CONNECTING TO OSC" : desktopBridgeAvailable ? "LINK STANDBY" : "NATIVE CLIENT REQUIRED"}</span><span className="mono">{preferences.networkMode === "multicast" ? preferences.multicastGroup : `${preferences.unicastAddress}:${preferences.unicastPort}`} : {preferences.listenerPort}</span></div>
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
            {roomSearch && (preferences.showRecentRooms || preferences.showRecommendedRooms) && recentRoomSuggestions.length === 0 && recommendedRoomSuggestions.length === 0 && <div className="no-room-matches">No matching rooms</div>}
          </div>
        </div>}

        {connected && <div className="mobile-roster roster-section"><div className="roster-heading"><span>ON CHANNEL</span><span className="roster-count">{visibleContacts.length + 1}</span></div><div className="roster-person"><div className="roster-avatar local">{identity.clientName.slice(0, 1).toUpperCase()}</div><div className="roster-copy"><strong>{identity.clientName || "Unnamed station"}</strong><span>YOU</span></div><span className="roster-presence" /></div>{visibleContacts.map((contact) => <div className="roster-person" key={contact.id}><div className="roster-avatar">{contact.name.slice(0, 1).toUpperCase()}</div><div className="roster-copy"><strong>{contact.name}</strong><span>{preferences.showPeerApplications ? contact.application : "ONLINE"}</span></div><span className="roster-presence" /></div>)}</div>}

        <div className="sidebar-bottom">
          <div className="network-status"><span className={connected ? "status-led active" : "status-led"} /><span>{connected ? "OSC NETWORK" : "OFFLINE"}</span><span className="mono">UDP</span></div>
          <div className="client-id"><span>CLIENT UUID</span><code>{clientUuid.slice(0, 8)}…{clientUuid.slice(-4)}</code></div>
        </div>
      </aside>

      <section className="main-panel">
        <header className="topbar">
          <div className="topbar-start"><button ref={mobileNavigationButtonRef} type="button" className="mobile-navigation-toggle" aria-label="Open navigation" aria-controls="station-navigation" aria-expanded={mobileNavigationOpen} title="Open navigation" onClick={() => setMobileNavigationOpen(true)}><Menu size={20} /></button><div className="channel-title"><div className="channel-icon">{settingsOpen ? <Settings2 size={18} /> : displayRoom ? <Hash size={18} /> : <Radio size={18} />}</div><div><h1>{settingsOpen ? "Settings" : displayRoom ? formatRoomName(displayRoom) : "Global channel"}</h1><p>{settingsOpen ? "CLIENT PREFERENCES" : activeRoom ? "ROOM TRANSMISSION" : sendToRoom ? "ROOM SEND ONLY" : "OPEN TRANSMISSION"}<span className="title-separator">/</span>{connected ? "LISTENING" : connecting ? "CONNECTING" : "NO SIGNAL"}</p></div></div></div>
          <div className="topbar-right"><span className="signal-label"><Activity size={14} /> {connected ? "SIGNAL READY" : connecting ? "CONNECTING" : "NO SIGNAL"}</span><span className="topbar-divider" /><span className="protocol-label">OSC <b>1.0</b></span><button type="button" className="settings-toggle" title={settingsOpen ? "Close settings" : "Settings"} aria-label={settingsOpen ? "Close settings" : "Settings"} onClick={() => { setMobileNavigationOpen(false); setSettingsOpen(!settingsOpen); setSettingsError(""); }}>{settingsOpen ? <X size={17} /> : <Settings2 size={17} />}</button></div>
        </header>

        {settingsOpen && <section className="settings-page">
          <div className="settings-intro"><button type="button" className="back-settings" onClick={() => setSettingsOpen(false)}><ArrowLeft size={15} /> Back to channel</button><span className="section-label">OSCMESSENGER DESKTOP</span><p>Network, identity, and display preferences for this device.</p>{nativeBridgeAvailable && <label className="settings-toggle-row browser-server-toggle"><span className="settings-toggle-copy"><strong>Serve browser interface</strong><small>Available only on this computer.</small></span><input type="checkbox" checked={preferences.serveWebInterface} onChange={(event) => updatePreference("serveWebInterface", event.target.checked)} /><span className="settings-toggle-track" aria-hidden="true" /></label>}{webInterfaceUrl && <div className="web-interface-address"><span>Browser interface</span><code>{webInterfaceUrl}</code></div>}</div>
          <form className="settings-form" onSubmit={saveSettings}>
            <section className="settings-group">
              <div className="settings-group-heading"><span className="settings-index">01</span><div><h2>Identity</h2><p>How this station appears to other clients.</p></div></div>
              <label className="settings-field"><span>Callsign</span><input value={identity.clientName} onChange={(event) => updateIdentity("clientName", event.target.value)} maxLength={48} autoComplete="off" /></label>
            </section>

            <section className="settings-group">
              <div className="settings-group-heading"><span className="settings-index">02</span><div><h2>Appearance</h2><p>Theme and interface accent.</p></div></div>
              <div className="settings-option-block"><span className="settings-option-label">Theme</span><div className="segmented-control"><button type="button" aria-pressed={preferences.theme === "dark"} onClick={() => updatePreference("theme", "dark")}><Moon size={15} /> Dark</button><button type="button" aria-pressed={preferences.theme === "light"} onClick={() => updatePreference("theme", "light")}><Sun size={15} /> Light</button></div></div>
              <div className="settings-option-block"><span className="settings-option-label">Accent color</span><div className="accent-options">{(["lime", "cyan", "amber", "rose"] as const).map((accent) => <button type="button" key={accent} className={`accent-choice accent-${accent} ${preferences.accent === accent ? "selected" : ""}`} aria-label={`${accent} accent`} aria-pressed={preferences.accent === accent} onClick={() => updatePreference("accent", accent)} title={`${accent[0].toUpperCase()}${accent.slice(1)}`}><span /></button>)}</div></div>
              <div className="settings-option-block"><span className="settings-option-label">Interface density</span><div className="segmented-control"><button type="button" aria-pressed={preferences.density === "comfortable"} onClick={() => updatePreference("density", "comfortable")}>Comfortable</button><button type="button" aria-pressed={preferences.density === "compact"} onClick={() => updatePreference("density", "compact")}>Compact</button></div></div>
            </section>

            <section className="settings-group">
              <div className="settings-group-heading"><span className="settings-index">03</span><div><h2>Behavior</h2><p>Connection and chat preferences.</p></div></div>
              <label className="settings-toggle-row"><span className="settings-toggle-copy"><strong>Connect on launch</strong><small>Attempt the OSC connection automatically when the desktop app starts.</small></span><input type="checkbox" checked={preferences.autoConnect} onChange={(event) => updatePreference("autoConnect", event.target.checked)} /><span className="settings-toggle-track" aria-hidden="true" /></label>
              <label className="settings-toggle-row"><span className="settings-toggle-copy"><strong>Enter sends message</strong><small>Use Shift+Enter to insert a new line.</small></span><input type="checkbox" checked={preferences.enterToSend} onChange={(event) => updatePreference("enterToSend", event.target.checked)} /><span className="settings-toggle-track" aria-hidden="true" /></label>
              <label className="settings-toggle-row"><span className="settings-toggle-copy"><strong>Show timestamps</strong><small>Display local receive time on each message.</small></span><input type="checkbox" checked={preferences.showTimestamps} onChange={(event) => updatePreference("showTimestamps", event.target.checked)} /><span className="settings-toggle-track" aria-hidden="true" /></label>
              <label className="settings-toggle-row"><span className="settings-toggle-copy"><strong>Show peer applications</strong><small>Show the application name advertised by other stations.</small></span><input type="checkbox" checked={preferences.showPeerApplications} onChange={(event) => updatePreference("showPeerApplications", event.target.checked)} /><span className="settings-toggle-track" aria-hidden="true" /></label>
              <label className="settings-toggle-row"><span className="settings-toggle-copy"><strong>Reduce motion</strong><small>Limit interface animations and smooth scrolling.</small></span><input type="checkbox" checked={preferences.reduceMotion} onChange={(event) => updatePreference("reduceMotion", event.target.checked)} /><span className="settings-toggle-track" aria-hidden="true" /></label>
            </section>

            <section className="settings-group">
              <div className="settings-group-heading"><span className="settings-index">04</span><div><h2>Gateway server</h2><p>Make this client available to TCP Gateway patrons.</p></div></div>
              <label className="settings-toggle-row"><span className="settings-toggle-copy"><strong>Manage a local Gateway</strong><small>Check for an active Gateway and start a local one if needed.</small></span><input type="checkbox" checked={preferences.gatewayEnabled} onChange={(event) => updatePreference("gatewayEnabled", event.target.checked)} /><span className="settings-toggle-track" aria-hidden="true" /></label>
              <label className="gateway-policy-field"><span>If another Gateway is active</span><select value={preferences.gatewayConflictPolicy} onChange={(event) => updatePreference("gatewayConflictPolicy", event.target.value as Preferences["gatewayConflictPolicy"])} disabled={!preferences.gatewayEnabled}><option value="start_anyway">Start anyway</option><option value="stop_local">Stop local server</option><option value="never_start">Never start local server</option></select></label>
              <div className="gateway-status-row"><span>Gateway status</span><strong className={`gateway-status-${gatewayStatus.state}`}>{!desktopBridgeAvailable ? "Native client required" : gatewayStatus.detail || ({ checking: "Checking network…", local_active: "Local Gateway active", remote_active: "Using another Gateway", disabled: "Local Gateway disabled", stopped: "Local Gateway stopped", error: "Gateway error", discovery_error: "Gateway discovery failed" }[gatewayStatus.state] ?? gatewayStatus.state)}</strong></div>
            </section>

            <section className="settings-group">
              <div className="settings-group-heading"><span className="settings-index">05</span><div><h2>Rooms</h2><p>Room suggestions and history.</p></div></div>
              <label className="settings-toggle-row"><span className="settings-toggle-copy"><strong>Show recent rooms</strong><small>Display recently joined rooms in the sidebar.</small></span><input type="checkbox" checked={preferences.showRecentRooms} onChange={(event) => updatePreference("showRecentRooms", event.target.checked)} /><span className="settings-toggle-track" aria-hidden="true" /></label>
              <label className="settings-toggle-row"><span className="settings-toggle-copy"><strong>Show recommended rooms</strong><small>Display suggested rooms in the sidebar.</small></span><input type="checkbox" checked={preferences.showRecommendedRooms} onChange={(event) => updatePreference("showRecommendedRooms", event.target.checked)} /><span className="settings-toggle-track" aria-hidden="true" /></label>
              <div className="settings-field-grid"><label className="settings-field"><span>Recent rooms displayed</span><input type="number" min="1" max={MAX_RECENT_ROOMS} step="1" value={preferences.recentRoomLimit} disabled={!preferences.showRecentRooms} onChange={(event) => { const value = event.target.valueAsNumber; if (Number.isInteger(value)) updatePreference("recentRoomLimit", Math.max(1, Math.min(MAX_RECENT_ROOMS, value))); }} /></label><label className="settings-field"><span>Recommended rooms displayed</span><input type="number" min="1" max={RECOMMENDED_ROOMS.length} step="1" value={preferences.recommendedRoomLimit} disabled={!preferences.showRecommendedRooms} onChange={(event) => { const value = event.target.valueAsNumber; if (Number.isInteger(value)) updatePreference("recommendedRoomLimit", Math.max(1, Math.min(RECOMMENDED_ROOMS.length, value))); }} /></label></div>
              <div className="history-clear-row"><div><strong>Recent rooms</strong><small>{recentRooms.length} of {MAX_RECENT_ROOMS} saved</small></div><button type="button" className="reset-network-button" disabled={recentRooms.length === 0} onClick={() => setRecentRooms([])}>Clear recent rooms</button></div>
            </section>

            <details className="settings-group advanced-network-settings">
              <summary className="settings-group-heading"><span className="settings-index">06</span><div><h2>Advanced settings</h2><p>Only change network settings if you understand your setup.</p></div></summary>
              <div className="advanced-settings-content">
                <p className="advanced-warning">Only change these settings if you understand your network setup. Incorrect values can prevent the client from connecting.</p>
                <div className="settings-option-block transport-choice"><span className="settings-option-label">Connection mode</span><div className="segmented-control"><button type="button" aria-pressed={preferences.networkMode === "multicast"} onClick={() => updatePreference("networkMode", "multicast")}><RadioTower size={15} /> Multicast</button><button type="button" aria-pressed={preferences.networkMode === "unicast"} onClick={() => updatePreference("networkMode", "unicast")}><Cable size={15} /> Unicast</button></div></div>
                {preferences.networkMode === "multicast" ? <>
                  <div className="settings-field-grid"><label className="settings-field"><span>Multicast group IPv4</span><input value={preferences.multicastGroup} onChange={(event) => updatePreference("multicastGroup", event.target.value)} inputMode="decimal" spellCheck={false} placeholder="239.254.0.1" /></label><label className="settings-field port-field"><span>Local listener port</span><input type="number" min="1" max="65535" step="1" value={preferences.listenerPort} onChange={(event) => updatePreference("listenerPort", event.target.value)} /></label></div>
                  <div className="interface-selection">
                    <div className="settings-option-block"><span className="settings-option-label">Receive interfaces</span><div className="segmented-control interface-mode-control"><button type="button" aria-pressed={preferences.interfaceMode === "default"} onClick={() => updatePreference("interfaceMode", "default")}>Any / default</button><button type="button" aria-pressed={preferences.interfaceMode === "all"} onClick={() => updatePreference("interfaceMode", "all")}>All</button><button type="button" aria-pressed={preferences.interfaceMode === "selected"} onClick={() => updatePreference("interfaceMode", "selected")}>Choose</button></div></div>
                    {preferences.interfaceMode === "default" && <p className="settings-note">Use the operating system’s default multicast interface.</p>}
                    {preferences.interfaceMode === "all" && <p className="settings-note">Join the multicast group on every detected IPv4 interface.</p>}
                    {preferences.interfaceMode === "selected" && <div className="interface-list">{networkInterfaces.map((networkInterface) => <label className="interface-option" key={`${networkInterface.name}-${networkInterface.address}`}><input type="checkbox" checked={preferences.multicastInterfaces.includes(networkInterface.address)} onChange={(event) => updatePreference("multicastInterfaces", event.target.checked ? [...new Set([...preferences.multicastInterfaces, networkInterface.address])] : preferences.multicastInterfaces.filter((address) => address !== networkInterface.address))} /><span><strong>{networkInterface.name}</strong><small>{networkInterface.address}{networkInterface.is_loopback ? " · loopback" : ""}</small></span></label>)}{networkInterfaces.length === 0 && <p className="settings-note">{!desktopBridgeAvailable ? "Open the native desktop client to enumerate local interfaces." : networkInterfaceError || "No local IPv4 interfaces found."}</p>}</div>}
                  </div>
                </> : <div className="settings-field-grid unicast-fields"><label className="settings-field"><span>Target device / gateway IPv4</span><input value={preferences.unicastAddress} onChange={(event) => updatePreference("unicastAddress", event.target.value)} inputMode="decimal" spellCheck={false} placeholder="192.168.1.50" /></label><label className="settings-field port-field"><span>Target UDP port</span><input type="number" min="1" max="65535" step="1" value={preferences.unicastPort} onChange={(event) => updatePreference("unicastPort", event.target.value)} /></label><label className="settings-field listener-field"><span>Local listener port</span><input type="number" min="1" max="65535" step="1" value={preferences.listenerPort} onChange={(event) => updatePreference("listenerPort", event.target.value)} /></label></div>}
                <div className="advanced-network-actions"><span>Network changes reconnect when saved.</span><button type="button" className="reset-network-button" onClick={resetNetworkSettings}>Reset to OSCMessenger defaults</button></div>
              </div>
            </details>

            {settingsError && <div className="settings-error" role="alert">{settingsError}</div>}
            <div className="settings-actions"><span>Preferences are saved on this device.</span><button type="submit" className="save-settings-button">Save settings</button></div>
          </form>
        </section>}

        {!settingsOpen && !desktopBridgeAvailable && <div className="browser-notice">Open OSCMessenger as a desktop app to connect to the multicast network.</div>}
        {!settingsOpen && notice && <div className="notice" role="status"><span>{notice}</span><button title="Dismiss" aria-label="Dismiss" onClick={() => setNotice("")}><X size={15} /></button></div>}
        {!settingsOpen && connected && droppedEvents > 0 && <div className="drop-warning"><ArrowDownLeft size={15} /> {droppedEvents} network event{droppedEvents === 1 ? "" : "s"} dropped while the queue was full.</div>}

        {!settingsOpen && (connected ? <div className="transcript" ref={transcriptRef}>
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
                  <div className="message-content"><div className="message-meta"><strong>{own ? `${identity.clientName} (you)` : contact?.name || message.senderId.slice(0, 8)}</strong>{preferences.showTimestamps && <span>{formatTime(message.receivedAt)}</span>}{!own && contact && preferences.showPeerApplications && <span className="app-tag">{contact.application}</span>}</div><p>{message.text}</p></div>
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
        </div>)}

        {!settingsOpen && connected && <footer className="composer-wrap">
          <form className="composer" onSubmit={sendMessage}>
            <div className="composer-leading"><span className="composer-dot" />{activeRoom ? <span>#{activeRoom} · {roomRole(activeRoom) ?? "Room"}</span> : <><select aria-label="Message destination" value={sendToRoom ? "room" : "global"} onChange={(event) => setSendToRoom(event.target.value === "room")} disabled={!connected}><option value="global">GLOBAL</option><option value="room">ROOM</option></select>{sendToRoom && <input className="target-room-input" aria-label="Room message destination" placeholder="room name" value={sendRoom} onChange={(event) => setSendRoom(event.target.value)} maxLength={64} disabled={!connected} />}</>}</div>
            <textarea value={messageDraft} onChange={(event) => setMessageDraft(event.target.value)} onKeyDown={submitOnEnter} placeholder={connected ? "Transmit a message…" : "Connect to transmit"} maxLength={2048} rows={1} disabled={!connected} aria-label="Message" />
            <button type="submit" className="send-button" aria-label="Send message" title="Send message" disabled={!connected || !messageDraft.trim() || sending}><Send size={17} /></button>
          </form>
          <div className="composer-hint"><span>RETURN TO SEND <kbd>↵</kbd></span><span>SHIFT + RETURN FOR NEW LINE</span>{sendToRoom && !activeRoom && <span className="direct-send-note">NOT JOINED</span>}<span className="char-count">{messageDraft.length}/2048</span></div>
        </footer>}
      </section>

      {connected && !settingsOpen && <aside className="details-panel">
          <div className="details-header"><div className="section-label">CHANNEL DETAILS</div><div className="details-channel"><div className="details-channel-icon">{displayRoom ? <Hash size={16} /> : <Radio size={16} />}</div><div><strong>{displayRoom ? formatRoomName(displayRoom) : "Global"}</strong><span>{activeRoom ? "ROOM" : sendToRoom ? "DIRECT TARGET" : "MULTICAST"}</span></div></div></div>
        <div className="roster-section"><div className="roster-heading"><span>ON CHANNEL</span><span className="roster-count">{visibleContacts.length + (connected ? 1 : 0)}</span></div>
          {connected && <div className="roster-person"><div className="roster-avatar local">{identity.clientName.slice(0, 1).toUpperCase()}</div><div className="roster-copy"><strong>{identity.clientName || "Unnamed station"}</strong><span>YOU</span></div><span className="roster-presence" /></div>}
          {visibleContacts.map((contact) => <div className="roster-person" key={contact.id}><div className="roster-avatar">{contact.name.slice(0, 1).toUpperCase()}</div><div className="roster-copy"><strong>{contact.name}</strong><span>{preferences.showPeerApplications ? contact.application : "ONLINE"}</span></div><span className="roster-presence" /></div>)}
          {!connected && <div className="roster-empty"><UsersRound size={17} /><span>No stations connected</span></div>}
          {connected && visibleContacts.length === 0 && <div className="roster-empty"><UsersRound size={17} /><span>Only you, for now</span></div>}
        </div>
        <div className="transport-section"><div className="section-label">TRANSPORT</div><div className="transport-row"><span>Mode</span><strong>{preferences.networkMode.toUpperCase()}</strong></div><div className="transport-row"><span>{preferences.networkMode === "multicast" ? "Group" : "Remote endpoint"}</span><strong className="mono">{preferences.networkMode === "multicast" ? preferences.multicastGroup : `${preferences.unicastAddress}:${preferences.unicastPort}`}</strong></div><div className="transport-row"><span>Local port</span><strong className="mono">{preferences.listenerPort}</strong></div><div className="transport-row"><span>Client ID</span><strong className="mono">{clientUuid.slice(0, 8)}</strong></div></div>
        <div className="details-footer"><DoorOpen size={14} /><span>{sendToRoom && !activeRoom ? "DIRECT ROOM SEND DOES NOT JOIN THE ROOM" : "ROOM MEMBERSHIP IS LOCAL TO THIS CLIENT"}</span></div>
      </aside>}
    </main>
  );
}