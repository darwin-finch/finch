import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Subscription } from "../models/components/subscription.js";
import { AlreadyCanceledSubscription } from "../models/errors/alreadycanceledsubscription.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { SubscriptionsUpdateRequest } from "../models/operations/subscriptionsupdate.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Update Subscription
 *
 * @remarks
 * Update a subscription.
 *
 * **Scopes**: `subscriptions:write`
 */
export declare function subscriptionsUpdate(client: PolarCore, request: SubscriptionsUpdateRequest, options?: RequestOptions): APIPromise<Result<Subscription, AlreadyCanceledSubscription | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=subscriptionsUpdate.d.ts.map