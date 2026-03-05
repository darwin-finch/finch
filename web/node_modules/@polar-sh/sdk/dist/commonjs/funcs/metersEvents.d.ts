import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { MetersEventsRequest, MetersEventsResponse } from "../models/operations/metersevents.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * Get Meter Events
 *
 * @remarks
 * Get events matching the filter of a meter.
 *
 * **Scopes**: `meters:read` `meters:write`
 */
export declare function metersEvents(client: PolarCore, request: MetersEventsRequest, options?: RequestOptions): APIPromise<PageIterator<Result<MetersEventsResponse, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=metersEvents.d.ts.map