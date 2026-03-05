import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { EventsIngest } from "../models/components/eventsingest.js";
import { EventsIngestResponse } from "../models/components/eventsingestresponse.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Ingest Events
 *
 * @remarks
 * Ingest batch of events.
 *
 * **Scopes**: `events:write`
 */
export declare function eventsIngest(client: PolarCore, request: EventsIngest, options?: RequestOptions): APIPromise<Result<EventsIngestResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=eventsIngest.d.ts.map