import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CheckoutLinksListRequest, CheckoutLinksListResponse } from "../models/operations/checkoutlinkslist.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * List Checkout Links
 *
 * @remarks
 * List checkout links.
 *
 * **Scopes**: `checkout_links:read` `checkout_links:write`
 */
export declare function checkoutLinksList(client: PolarCore, request: CheckoutLinksListRequest, options?: RequestOptions): APIPromise<PageIterator<Result<CheckoutLinksListResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=checkoutLinksList.d.ts.map