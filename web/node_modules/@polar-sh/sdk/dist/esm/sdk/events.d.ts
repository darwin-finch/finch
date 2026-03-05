import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { Event } from "../models/components/event.js";
import { EventsIngest } from "../models/components/eventsingest.js";
import { EventsIngestResponse } from "../models/components/eventsingestresponse.js";
import { EventsGetRequest } from "../models/operations/eventsget.js";
import { EventsListRequest, EventsListResponse } from "../models/operations/eventslist.js";
import { PageIterator } from "../types/operations.js";
export declare class Events extends ClientSDK {
    /**
     * List Events
     *
     * @remarks
     * List events.
     *
     * **Scopes**: `events:read` `events:write`
     */
    list(request: EventsListRequest, options?: RequestOptions): Promise<PageIterator<EventsListResponse, {
        page: number;
    }>>;
    /**
     * Get Event
     *
     * @remarks
     * Get an event by ID.
     *
     * **Scopes**: `events:read` `events:write`
     */
    get(request: EventsGetRequest, options?: RequestOptions): Promise<Event>;
    /**
     * Ingest Events
     *
     * @remarks
     * Ingest batch of events.
     *
     * **Scopes**: `events:write`
     */
    ingest(request: EventsIngest, options?: RequestOptions): Promise<EventsIngestResponse>;
}
//# sourceMappingURL=events.d.ts.map